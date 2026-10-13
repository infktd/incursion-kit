//! Ship fittings and the two formats pilots paste them in: DNA and EFT.

use std::collections::{BTreeMap, BTreeSet};

use wl_eve_data::{Category, EveData, TypeId};

/// A fitted ship: hull, fitted modules (including drones) and cargo, each with counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fitting {
    pub hull: TypeId,
    pub modules: BTreeMap<TypeId, i64>,
    pub cargo: BTreeMap<TypeId, i64>,
}

/// A fit could not be parsed or is not a valid fit. Messages match the legacy waitlist, whose API
/// error bodies expose them (`fit error: unknown item type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FitError {
    #[error("fit failed to parse")]
    Parse,
    #[error("unknown item type")]
    InvalidModule,
    #[error("invalid item count")]
    InvalidCount,
    #[error("only ships can fly")]
    InvalidHull,
}

impl From<std::num::ParseIntError> for FitError {
    fn from(_: std::num::ParseIntError) -> Self {
        Self::Parse
    }
}

/// DNA strings with more items than this are rejected.
const MAX_DNA_ITEMS: usize = 1000;

/// EFT sections are high, med, low, rig, subsystem, drone, then cargo: a blank line ends each.
const EFT_CARGO_SECTION: usize = 7;

fn add(section: &mut BTreeMap<TypeId, i64>, type_id: TypeId, count: i64) -> Result<(), FitError> {
    let entry = section.entry(type_id).or_insert(0);
    *entry = entry.checked_add(count).ok_or(FitError::InvalidCount)?;
    Ok(())
}

impl Fitting {
    /// Parses ship DNA: `hull:type;count:type;count:...::`. A type id ending in `_` is cargo;
    /// charges and implants always go to cargo.
    pub fn from_dna(dna: &str, eve: &EveData) -> Result<Self, FitError> {
        let mut pieces = dna.split(':');
        let hull: TypeId = pieces.next().unwrap_or_default().parse()?;

        let mut fit = Self {
            hull,
            modules: BTreeMap::new(),
            cargo: BTreeMap::new(),
        };

        for (index, piece) in pieces.filter(|piece| !piece.is_empty()).enumerate() {
            if index >= MAX_DNA_ITEMS {
                return Err(FitError::Parse);
            }

            let (type_str, count_str) = match piece.split_once(';') {
                Some((type_str, count)) => (type_str, Some(count)),
                None => (piece, None),
            };
            let (type_str, marked_cargo) = match type_str.strip_suffix('_') {
                Some(stripped) => (stripped, true),
                None => (type_str, false),
            };
            let type_id: TypeId = type_str.parse()?;
            let count: i64 = match count_str {
                None => 1,
                Some(count) => count.parse()?,
            };

            let is_cargo = marked_cargo
                || eve
                    .get(type_id)
                    .ok_or(FitError::InvalidModule)?
                    .is_always_cargo();
            let section = if is_cargo {
                &mut fit.cargo
            } else {
                &mut fit.modules
            };
            add(section, type_id, count)?;
        }

        Ok(fit)
    }

    /// Renders ship DNA. Modules come first (by type id), then cargo; cargo that is not a charge
    /// or implant is marked with `_`.
    pub fn to_dna(&self, eve: &EveData) -> Result<String, FitError> {
        let mut dna = format!("{}:", self.hull);
        for (id, count) in &self.modules {
            dna += &format!("{id};{count}:");
        }
        for (&id, count) in &self.cargo {
            let ty = eve.get(id).ok_or(FitError::InvalidModule)?;
            if ty.is_always_cargo() {
                dna += &format!("{id};{count}:");
            } else {
                dna += &format!("{id}_;{count}:");
            }
        }
        Ok(dna + ":")
    }

    /// Parses one or more fits in EFT format (as copied from the game or a fitting tool).
    ///
    /// Each fit starts with a `[Hull, Name]` line. Blank lines separate the slot sections;
    /// everything from the seventh section on is cargo. Stacked items (`Name x3`) are cargo
    /// unless they are drones; charges and implants are always cargo. Text before the first
    /// header (even a blank line) is an error.
    pub fn from_eft(eft: &str, eve: &EveData) -> Result<Vec<Self>, FitError> {
        let mut fittings: Vec<Self> = Vec::new();
        let mut section = 0;

        for line in eft.lines() {
            let line = line.trim();

            if line.starts_with('[') && line.ends_with(']') && line.contains(',') {
                let inner = &line[1..line.len() - 1];
                let (hull_name, _ship_name) = inner.split_once(',').ok_or(FitError::Parse)?;
                let hull = eve.id_of(hull_name.trim()).ok_or(FitError::InvalidModule)?;
                fittings.push(Self {
                    hull,
                    modules: BTreeMap::new(),
                    cargo: BTreeMap::new(),
                });
                section = 0;
                continue;
            }

            let Some(fit) = fittings.last_mut() else {
                return Err(FitError::Parse);
            };
            if line.starts_with("[Empty ") {
                continue;
            }
            if line.is_empty() {
                section += 1;
                continue;
            }

            let mut pieces = line.split(" x");
            let type_name = pieces.next().unwrap_or_default();
            let type_id = eve.id_of(type_name).ok_or(FitError::InvalidModule)?;
            let (count, stacked) = match pieces.next() {
                None => (1, false),
                Some(count) => (count.parse()?, true),
            };

            let is_cargo = section >= EFT_CARGO_SECTION || {
                let ty = eve.get(type_id).ok_or(FitError::InvalidModule)?;
                ty.is_always_cargo() || (stacked && ty.category != Category::Drone)
            };
            let destination = if is_cargo {
                &mut fit.cargo
            } else {
                &mut fit.modules
            };
            add(destination, type_id, count)?;
        }

        Ok(fittings)
    }

    /// Checks that counts are positive, every type exists, nothing that must be cargo is fitted
    /// and the hull is a ship.
    pub fn validate(&self, eve: &EveData) -> Result<(), FitError> {
        let mut ids = BTreeSet::from([self.hull]);
        for (&id, &count) in self.modules.iter().chain(&self.cargo) {
            ids.insert(id);
            if count <= 0 {
                return Err(FitError::InvalidCount);
            }
        }

        if ids.iter().any(|&id| eve.get(id).is_none()) {
            return Err(FitError::InvalidModule);
        }
        if self
            .modules
            .keys()
            .any(|&id| eve.get(id).is_some_and(|ty| ty.is_always_cargo()))
        {
            return Err(FitError::InvalidModule);
        }
        if eve
            .get(self.hull)
            .is_some_and(|hull| hull.category != Category::Ship)
        {
            return Err(FitError::InvalidHull);
        }
        Ok(())
    }

    /// Every type id on the fit: hull, modules and cargo.
    pub fn type_ids(&self) -> impl Iterator<Item = TypeId> + '_ {
        std::iter::once(self.hull)
            .chain(self.modules.keys().copied())
            .chain(self.cargo.keys().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_eve_data::{EffectId, EffectRow, Tables, TypeRow};

    const HULL: TypeId = 100;
    const LASER: TypeId = 200;
    const PLATE: TypeId = 201;
    const AMMO: TypeId = 300;
    const DRONE: TypeId = 400;
    const IMPLANT: TypeId = 500;
    const STRUCTURE: TypeId = 600;

    /// A tiny static data set: a hull, two modules, a charge, a drone, an implant and a
    /// non-ship "hull".
    fn eve() -> EveData {
        let row = |id, name: &str, category_id| TypeRow {
            id,
            name: name.into(),
            group_id: 1,
            category_id: Some(category_id),
            published: true,
        };
        EveData::from_tables(
            Tables {
                types: vec![
                    row(HULL, "Nightmare", 6),
                    row(LASER, "Mega Pulse Laser II", 7),
                    row(PLATE, "1600mm Steel Plates II", 7),
                    row(AMMO, "Conflagration L", 8),
                    row(DRONE, "Hobgoblin II", 18),
                    row(IMPLANT, "High-grade Amulet Alpha", 20),
                    row(STRUCTURE, "Astrahus", 65),
                ],
                effects: vec![
                    EffectRow {
                        type_id: LASER,
                        effect: EffectId::HIGH_POWER,
                    },
                    EffectRow {
                        type_id: PLATE,
                        effect: EffectId::LOW_POWER,
                    },
                ],
                ..Tables::default()
            },
            None,
        )
    }

    fn map(entries: &[(TypeId, i64)]) -> BTreeMap<TypeId, i64> {
        entries.iter().copied().collect()
    }

    #[test]
    fn dna_round_trip() {
        let eve = eve();
        let fit = Fitting::from_dna("100::", &eve).unwrap();
        assert_eq!(fit.hull, HULL);
        assert!(fit.modules.is_empty() && fit.cargo.is_empty());
        assert_eq!(fit.to_dna(&eve).unwrap(), "100::");

        // Charges go to cargo by category, `_` marks other cargo, counts default to 1 and add up.
        let fit = Fitting::from_dna("100:201;2:200:300;40:201_;1:200;3::", &eve).unwrap();
        assert_eq!(fit.modules, map(&[(LASER, 4), (PLATE, 2)]));
        assert_eq!(fit.cargo, map(&[(PLATE, 1), (AMMO, 40)]));
        assert_eq!(fit.to_dna(&eve).unwrap(), "100:200;4:201;2:201_;1:300;40::");
    }

    #[test]
    fn dna_errors() {
        let eve = eve();
        assert_eq!(Fitting::from_dna("", &eve), Err(FitError::Parse));
        assert_eq!(Fitting::from_dna("abc::", &eve), Err(FitError::Parse));
        assert_eq!(Fitting::from_dna("100:200;x::", &eve), Err(FitError::Parse));
        assert_eq!(
            Fitting::from_dna("100:999;1::", &eve),
            Err(FitError::InvalidModule)
        );
        // Unknown cargo marked with `_` parses, and validation rejects it.
        let fit = Fitting::from_dna("100:999_;1::", &eve).unwrap();
        assert_eq!(fit.validate(&eve), Err(FitError::InvalidModule));
        assert_eq!(fit.to_dna(&eve), Err(FitError::InvalidModule));

        let too_long = format!("100:{}:", vec!["200;1"; 1001].join(":"));
        assert_eq!(Fitting::from_dna(&too_long, &eve), Err(FitError::Parse));
        let just_fits = format!("100:{}:", vec!["200;1"; 1000].join(":"));
        assert!(Fitting::from_dna(&just_fits, &eve).is_ok());
    }

    #[test]
    fn validation() {
        let eve = eve();
        let valid = Fitting::from_dna("100:200;1:300;1::", &eve).unwrap();
        assert_eq!(valid.validate(&eve), Ok(()));
        let zero = Fitting::from_dna("100:200;0::", &eve).unwrap();
        assert_eq!(zero.validate(&eve), Err(FitError::InvalidCount));
        let negative = Fitting::from_dna("100:200;-1::", &eve).unwrap();
        assert_eq!(negative.validate(&eve), Err(FitError::InvalidCount));
        let not_a_ship = Fitting::from_dna("600:200;1::", &eve).unwrap();
        assert_eq!(not_a_ship.validate(&eve), Err(FitError::InvalidHull));
        let unknown_hull = Fitting::from_dna("1:200;1::", &eve).unwrap();
        assert_eq!(unknown_hull.validate(&eve), Err(FitError::InvalidModule));
        let fitted_charge = Fitting {
            hull: HULL,
            modules: map(&[(AMMO, 1)]),
            cargo: BTreeMap::new(),
        };
        assert_eq!(fitted_charge.validate(&eve), Err(FitError::InvalidModule));
    }

    #[test]
    fn eft_sections_and_stacks() {
        let eve = eve();
        let fits = Fitting::from_eft(
            "[Nightmare, My Nightmare]\n\n\nMega Pulse Laser II\nMega Pulse Laser II\n[Empty High slot]\n\n\n\n\n\nConflagration L x2\n1600mm Steel Plates II x1\n",
            &eve,
        )
        .unwrap();
        assert_eq!(fits.len(), 1);
        assert_eq!(fits[0].modules, map(&[(LASER, 2)]));
        assert_eq!(fits[0].cargo, map(&[(PLATE, 1), (AMMO, 2)]));

        // Stacked drones in the drone section are fitted; stacked modules are cargo; implants
        // and charges are always cargo.
        let fit = Fitting::from_eft(
            "  [Nightmare, x]  \n1600mm Steel Plates II x2\nHobgoblin II x5\nHigh-grade Amulet Alpha\nConflagration L\n",
            &eve,
        )
        .unwrap()
        .remove(0);
        assert_eq!(fit.modules, map(&[(DRONE, 5)]));
        assert_eq!(fit.cargo, map(&[(PLATE, 2), (AMMO, 1), (IMPLANT, 1)]));
    }

    #[test]
    fn eft_multiple_fits() {
        let eve = eve();
        let fits = Fitting::from_eft(
            "[Nightmare, one]\nMega Pulse Laser II\n\n[Nightmare, two, with a comma]\n1600mm Steel Plates II\n",
            &eve,
        )
        .unwrap();
        assert_eq!(fits.len(), 2);
        assert_eq!(fits[0].modules, map(&[(LASER, 1)]));
        assert_eq!(fits[1].modules, map(&[(PLATE, 1)]));
        assert_eq!(Fitting::from_eft("", &eve), Ok(Vec::new()));
    }

    #[test]
    fn eft_errors() {
        let eve = eve();
        // Anything before the first header, even a blank line, is an error.
        assert_eq!(
            Fitting::from_eft("\n[Nightmare, x]\n", &eve),
            Err(FitError::Parse)
        );
        assert_eq!(
            Fitting::from_eft("Mega Pulse Laser II\n", &eve),
            Err(FitError::Parse)
        );
        // A header needs a comma.
        assert_eq!(
            Fitting::from_eft("[Nightmare]\n", &eve),
            Err(FitError::Parse)
        );
        assert_eq!(
            Fitting::from_eft("[Imaginary, x]\n", &eve),
            Err(FitError::InvalidModule)
        );
        assert_eq!(
            Fitting::from_eft("[Nightmare, x]\nMega Pulse Laser III\n", &eve),
            Err(FitError::InvalidModule)
        );
        // Loaded charges are not supported (exact type names only).
        assert_eq!(
            Fitting::from_eft(
                "[Nightmare, x]\nMega Pulse Laser II, Conflagration L\n",
                &eve
            ),
            Err(FitError::InvalidModule)
        );
        assert_eq!(
            Fitting::from_eft("[Nightmare, x]\nConflagration L xmany\n", &eve),
            Err(FitError::Parse)
        );
        // Names are case-sensitive.
        assert_eq!(
            Fitting::from_eft("[Nightmare, x]\nmega pulse laser ii\n", &eve),
            Err(FitError::InvalidModule)
        );
    }

    #[test]
    fn error_messages_match_the_api() {
        assert_eq!(FitError::Parse.to_string(), "fit failed to parse");
        assert_eq!(FitError::InvalidModule.to_string(), "unknown item type");
        assert_eq!(FitError::InvalidCount.to_string(), "invalid item count");
        assert_eq!(FitError::InvalidHull.to_string(), "only ships can fly");
    }
}
