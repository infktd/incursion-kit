//! Per-system roles of an incursion: staging, vanguard, assault and headquarters.
//!
//! ESI only names the staging system and lists the infested systems. The other roles come from:
//!
//! 1. **Known layouts** (`data/layouts.json`): the roles eve-incursions.de recorded per
//!    constellation (its `solar_systems.systemType` seed data, MIT licensed; see `NOTICE`). A
//!    layout is used when its staging system is the one ESI reports and it names a role for
//!    every infested system. Role source `known`.
//! 2. **An estimate from the stargate graph** otherwise. Count jumps from the staging system
//!    through the constellation (and the infested systems); the farthest system is the headquarters, the nearest
//!    `ceil(n / 2)` of the other `n` systems are vanguards, the rest assaults (ties: lower
//!    security first, then system id). Checked against the 133 complete known layouts this gets
//!    84 % of systems and 73 % of headquarters right. Role source `estimated`.
//! 3. Without the star map (an old static data file), the non-staging systems are `unknown`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use wl_eve_data::Universe;

/// The role of an infested system.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Staging,
    Vanguard,
    Assault,
    Headquarters,
    Unknown,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Vanguard => "vanguard",
            Self::Assault => "assault",
            Self::Headquarters => "headquarters",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "staging" => Self::Staging,
            "vanguard" => Self::Vanguard,
            "assault" => Self::Assault,
            "headquarters" => Self::Headquarters,
            _ => Self::Unknown,
        }
    }
}

/// Where a role came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleSource {
    /// ESI (staging) or a published layout.
    Known,
    /// Derived from the stargate graph.
    Estimated,
    Unknown,
}

impl RoleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "known" => Self::Known,
            "estimated" => Self::Estimated,
            _ => Self::Unknown,
        }
    }
}

/// A recorded layout of one constellation.
#[derive(Debug, Clone, Deserialize)]
pub struct Layout {
    pub constellation_id: i64,
    pub name: String,
    pub staging: i64,
    pub vanguard: Vec<i64>,
    pub assault: Vec<i64>,
    pub headquarters: Vec<i64>,
}

impl Layout {
    fn role_of(&self, system_id: i64) -> Option<Role> {
        if system_id == self.staging {
            Some(Role::Staging)
        } else if self.vanguard.contains(&system_id) {
            Some(Role::Vanguard)
        } else if self.assault.contains(&system_id) {
            Some(Role::Assault)
        } else if self.headquarters.contains(&system_id) {
            Some(Role::Headquarters)
        } else {
            None
        }
    }
}

#[derive(Deserialize)]
struct LayoutFile {
    constellations: Vec<Layout>,
}

/// The known layouts by constellation id.
pub fn known_layouts() -> &'static HashMap<i64, Layout> {
    static LAYOUTS: OnceLock<HashMap<i64, Layout>> = OnceLock::new();
    LAYOUTS.get_or_init(|| {
        let file: LayoutFile = serde_json::from_str(include_str!("../data/layouts.json"))
            .expect("data/layouts.json is valid");
        file.constellations
            .into_iter()
            .map(|layout| (layout.constellation_id, layout))
            .collect()
    })
}

/// The role of every infested system (in the order given; the staging system is added when ESI
/// left it out of the list).
pub fn assign(
    constellation_id: i64,
    staging: i64,
    infested: &[i64],
    universe: &Universe,
) -> Vec<(i64, Role, RoleSource)> {
    let mut systems: Vec<i64> = Vec::with_capacity(infested.len() + 1);
    if !infested.contains(&staging) {
        systems.push(staging);
    }
    for &id in infested {
        if !systems.contains(&id) {
            systems.push(id);
        }
    }

    if let Some(layout) = known_layouts().get(&constellation_id)
        && layout.staging == staging
        && systems.iter().all(|&id| layout.role_of(id).is_some())
    {
        return systems
            .into_iter()
            .map(|id| {
                (
                    id,
                    layout.role_of(id).unwrap_or(Role::Unknown),
                    RoleSource::Known,
                )
            })
            .collect();
    }

    let estimated = estimate(staging, &systems, universe);
    systems
        .into_iter()
        .map(|id| {
            if id == staging {
                (id, Role::Staging, RoleSource::Known)
            } else {
                match estimated.get(&id) {
                    Some(&role) => (id, role, RoleSource::Estimated),
                    None => (id, Role::Unknown, RoleSource::Unknown),
                }
            }
        })
        .collect()
}

/// The stargate-graph estimate (see the module docs); empty without map data.
fn estimate(staging: i64, systems: &[i64], universe: &Universe) -> HashMap<i64, Role> {
    let mut roles = HashMap::new();
    let Some(staging_system) = universe.system(staging) else {
        return roles;
    };
    let constellation = staging_system.constellation_id;
    let infested: HashSet<i64> = systems.iter().copied().collect();
    let inside = universe.jumps_from(staging, |s| {
        s.constellation_id == constellation || infested.contains(&s.id)
    });
    let anywhere = universe.jumps_from(staging, |_| true);
    let security = |id: i64| universe.system(id).map_or(0.0, |s| s.security);
    let distance = |id: i64| {
        inside
            .to(id)
            .or_else(|| anywhere.to(id).map(|d| d + 100))
            .unwrap_or(u32::MAX)
    };

    let mut others: Vec<i64> = systems
        .iter()
        .copied()
        .filter(|&id| id != staging && universe.system(id).is_some())
        .collect();
    if others.is_empty() {
        return roles;
    }
    let n = others.len();
    // Nearest first; among equals the lower security first.
    others.sort_by(|&a, &b| {
        distance(a)
            .cmp(&distance(b))
            .then(security(a).total_cmp(&security(b)))
            .then(a.cmp(&b))
    });
    // The headquarters: the farthest, preferring the lowest security among the farthest.
    let farthest = distance(*others.last().expect("not empty"));
    let headquarters = others
        .iter()
        .copied()
        .filter(|&id| distance(id) == farthest)
        .min_by(|&a, &b| security(a).total_cmp(&security(b)).then(a.cmp(&b)))
        .expect("not empty");
    roles.insert(headquarters, Role::Headquarters);
    let vanguards = if n > 1 { n.div_ceil(2) } else { 0 };
    for (index, id) in others
        .into_iter()
        .filter(|&id| id != headquarters)
        .enumerate()
    {
        let role = if index < vanguards {
            Role::Vanguard
        } else {
            Role::Assault
        };
        roles.insert(id, role);
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_eve_data::universe::{Constellation, MapTables, Region, SolarSystem};

    fn universe() -> Universe {
        // A chain 1-2-3-4-5-6-7 in one constellation, security falling along it.
        let systems = (1..=7)
            .map(|id| SolarSystem {
                id,
                name: format!("S{id}"),
                constellation_id: 10,
                region_id: 1,
                security: 1.0 - id as f64 / 10.0,
                radius_au: 1.0,
                longest_warp_au: 1.0,
            })
            .collect();
        Universe::from_tables(MapTables {
            regions: vec![Region {
                id: 1,
                name: "R".into(),
            }],
            constellations: vec![Constellation {
                id: 10,
                name: "C".into(),
                region_id: 1,
                systems: vec![],
            }],
            systems,
            jumps: (1..7).flat_map(|a| [(a, a + 1), (a + 1, a)]).collect(),
            stations: vec![],
        })
    }

    #[test]
    fn layouts_load_and_are_consistent() {
        let layouts = known_layouts();
        assert!(layouts.len() > 100, "{}", layouts.len());
        let san_matar = &layouts[&20000001];
        assert_eq!(san_matar.name, "San Matar");
        assert_eq!(san_matar.headquarters, [30000002]);
        for layout in layouts.values() {
            assert!(layout.headquarters.len() <= 1, "{}", layout.name);
        }
    }

    #[test]
    fn uses_a_matching_known_layout() {
        let layout = &known_layouts()[&20000001];
        let mut infested = layout.vanguard.clone();
        infested.extend(&layout.assault);
        infested.extend(&layout.headquarters);
        infested.push(layout.staging);
        let roles = assign(20000001, layout.staging, &infested, &Universe::default());
        assert!(
            roles
                .iter()
                .all(|(_, _, source)| *source == RoleSource::Known)
        );
        assert!(roles.contains(&(30000002, Role::Headquarters, RoleSource::Known)));

        // Another staging system: the layout no longer applies; without a map, unknown.
        let roles = assign(20000001, 30000002, &infested, &Universe::default());
        assert!(roles.contains(&(30000002, Role::Staging, RoleSource::Known)));
        assert!(
            roles
                .iter()
                .filter(|(id, _, _)| *id != 30000002)
                .all(|(_, role, source)| *role == Role::Unknown && *source == RoleSource::Unknown)
        );
    }

    #[test]
    fn estimates_from_the_stargate_graph() {
        let universe = universe();
        let roles: HashMap<i64, Role> = assign(10, 1, &[1, 2, 3, 4, 5, 6, 7], &universe)
            .into_iter()
            .map(|(id, role, source)| {
                if id != 1 {
                    assert_eq!(source, RoleSource::Estimated);
                }
                (id, role)
            })
            .collect();
        assert_eq!(roles[&1], Role::Staging);
        assert_eq!(roles[&7], Role::Headquarters);
        // 6 other systems: 3 vanguards (nearest), 2 assaults.
        assert_eq!(roles[&2], Role::Vanguard);
        assert_eq!(roles[&3], Role::Vanguard);
        assert_eq!(roles[&4], Role::Vanguard);
        assert_eq!(roles[&5], Role::Assault);
        assert_eq!(roles[&6], Role::Assault);
    }

    #[test]
    fn staging_missing_from_the_list_is_added() {
        let roles = assign(10, 1, &[2, 3], &universe());
        assert_eq!(roles[0], (1, Role::Staging, RoleSource::Known));
        assert_eq!(roles.len(), 3);
    }
}
