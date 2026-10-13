//! `modules.yaml`: which modules may replace which (and whether that is an upgrade or a
//! downgrade), which modules identify a doctrine fit, cargo the fit checker ignores, booster
//! overrides, per-hull fit exceptions and SRP price overrides.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use wl_eve_data::{AttributeId, NameResolver, TypeId};

use crate::{DataError, DataFile, yaml};

const FILE: &str = "modules.yaml";

/// Meta level given to the abyssal and alternative modules of a `from_meta` entry: above any
/// regular meta level, so they count as upgrades.
const ABYSSAL_META_LEVEL: i64 = 17;

/// Attribute values closer than this belong to the same tier (compared as 32-bit floats, as the
/// legacy waitlist did).
const ATTRIBUTE_TIER_EPSILON: f32 = 1e-10;

#[derive(Deserialize)]
struct ModulesFile {
    alternatives: Vec<Vec<Vec<String>>>,
    from_meta: Vec<FromMetaEntry>,
    from_attribute: Vec<FromAttributeEntry>,
    accept_t1: Vec<String>,
    cargo_ignore: Vec<String>,
    identification: Vec<String>,
    drugs_approve_override: Vec<DrugOverrideEntry>,
    fit_variation_rules: Vec<FitVariationEntry>,
    #[serde(default)]
    srp_modified: Vec<SrpModifiedEntry>,
}

#[derive(Deserialize)]
struct FromMetaEntry {
    base: String,
    abyssal: Option<String>,
    alternative: Option<String>,
}

#[derive(Deserialize)]
struct FromAttributeEntry {
    base: Vec<String>,
    attribute: i32,
    reverse: Option<bool>,
}

#[derive(Deserialize)]
struct ModuleCount {
    name: String,
    amount: i64,
    /// Only for `fit_variation_rules`: `==` (default), `<=`, `>=`, `<`, `>` or their spelled-out
    /// forms.
    #[serde(default)]
    compare: Option<String>,
}

#[derive(Deserialize)]
struct DrugOverrideEntry {
    detect: String,
    remove: Vec<String>,
    add: Vec<ModuleCount>,
}

#[derive(Deserialize)]
struct FitVariationEntry {
    hull: String,
    missing: Option<Vec<ModuleCount>>,
    extra: Option<Vec<ModuleCount>>,
}

#[derive(Deserialize)]
struct SrpModifiedEntry {
    name: String,
    #[serde(default)]
    market_percent: Option<f64>,
    #[serde(default)]
    flat: Option<i64>,
    #[serde(default)]
    hulls: Vec<String>,
    #[serde(default)]
    exclude_hulls: Vec<String>,
}

/// One acceptable replacement for a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variation {
    pub from: TypeId,
    pub to: TypeId,
    /// Positive: `to` is better than `from`; negative: worse; zero: equivalent.
    pub meta_diff: i64,
}

/// Module replacement table.
#[derive(Debug, Clone, Default)]
pub struct Variator {
    variations: BTreeMap<TypeId, Vec<Variation>>,
    /// Cargo the fit checker does not require.
    pub cargo_ignore: BTreeSet<TypeId>,
}

impl Variator {
    /// Replacements for a module (including itself), best first: equal, then upgrades by
    /// increasing meta difference, then downgrades by increasing distance. `None` when the
    /// module has no declared replacements.
    pub fn get(&self, from: TypeId) -> Option<&[Variation]> {
        self.variations.get(&from).map(Vec::as_slice)
    }

    /// Declares every module of `tiers` replaceable by every other, with the tier difference as
    /// the meta difference.
    fn merge_tiers(
        &mut self,
        tiers: &BTreeMap<TypeId, i64>,
        names: &NameResolver<'_>,
    ) -> Result<(), DataError> {
        for (&from, &from_tier) in tiers {
            if self.variations.contains_key(&from) {
                let name = names.eve().name_of(from).unwrap_or("?");
                return Err(DataError::invalid(
                    FILE,
                    format!("{name} ({from}) is declared in more than one module group"),
                ));
            }
            let mut variations: Vec<Variation> = tiers
                .iter()
                .map(|(&to, &to_tier)| Variation {
                    from,
                    to,
                    meta_diff: to_tier - from_tier,
                })
                .collect();
            variations.sort_by_key(|variation| {
                if variation.meta_diff < 0 {
                    1_000_000 - variation.meta_diff
                } else {
                    variation.meta_diff
                }
            });
            self.variations.insert(from, variations);
        }
        Ok(())
    }
}

/// How a fit-variation rule compares a count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountCompare {
    Eq,
    Lte,
    Gte,
    Lt,
    Gt,
}

impl CountCompare {
    fn parse(raw: Option<&str>) -> Option<Self> {
        let raw = raw.map(|raw| raw.trim().to_ascii_lowercase());
        Some(match raw.as_deref() {
            None | Some("" | "==" | "eq" | "=") => Self::Eq,
            Some("<=" | "lte" | "max") => Self::Lte,
            Some(">=" | "gte" | "min") => Self::Gte,
            Some("<" | "lt") => Self::Lt,
            Some(">" | "gt") => Self::Gt,
            Some(_) => return None,
        })
    }

    pub fn matches(self, actual: i64, amount: i64) -> bool {
        match self {
            Self::Eq => actual == amount,
            Self::Lte => actual <= amount,
            Self::Gte => actual >= amount,
            Self::Lt => actual < amount,
            Self::Gt => actual > amount,
        }
    }
}

/// A count condition of a fit-variation rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariationCount {
    pub amount: i64,
    pub compare: CountCompare,
}

impl VariationCount {
    /// An exact (`==`) condition requires the module to be in the diff at all; the inequalities
    /// treat an absent module as a count of 0, so several "up to N" conditions combine cleanly.
    pub fn matches(&self, actual: Option<&i64>) -> bool {
        match (self.compare, actual) {
            (CountCompare::Eq, None) => false,
            (compare, actual) => compare.matches(actual.copied().unwrap_or(0), self.amount),
        }
    }
}

/// A per-hull exception: when the differences against the doctrine fit are exactly these, they
/// are forgiven.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FitVariation {
    pub missing: BTreeMap<TypeId, VariationCount>,
    pub extra: BTreeMap<TypeId, VariationCount>,
}

/// When a doctrine fit carries `detect`, its expected cargo drops `remove` and requires `add`
/// instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrugOverride {
    pub add: BTreeMap<TypeId, i64>,
    pub remove: BTreeSet<TypeId>,
}

/// How SRP values a module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SrpModifiedPricing {
    /// Percent of the market sell value (50 = 50%).
    MarketPercent(f64),
    /// Fixed ISK per module.
    Flat(i64),
}

/// An SRP price override for a module. Informational for the fit checker.
#[derive(Debug, Clone, PartialEq)]
pub struct SrpModifiedRule {
    pub pricing: SrpModifiedPricing,
    /// When not empty, only these hulls.
    pub hulls: BTreeSet<TypeId>,
    pub exclude_hulls: BTreeSet<TypeId>,
}

impl SrpModifiedRule {
    /// Whether the rule applies to a lost ship. An unknown hull only matches unrestricted rules.
    pub fn applies_to_hull(&self, hull: Option<TypeId>) -> bool {
        match hull {
            None => self.hulls.is_empty() && self.exclude_hulls.is_empty(),
            Some(hull) => {
                (self.hulls.is_empty() || self.hulls.contains(&hull))
                    && !self.exclude_hulls.contains(&hull)
            }
        }
    }
}

/// Everything `modules.yaml` defines.
#[derive(Debug, Clone, Default)]
pub struct ModuleRules {
    pub variator: Variator,
    /// Modules (and their replacements) that weigh heavily when matching a fit to a doctrine fit.
    pub identification: BTreeSet<TypeId>,
    /// Keyed by the detected cargo item.
    pub drug_overrides: BTreeMap<TypeId, DrugOverride>,
    /// Keyed by hull.
    pub fit_variations: BTreeMap<TypeId, Vec<FitVariation>>,
    /// Keyed by module.
    pub srp_modified: BTreeMap<TypeId, SrpModifiedRule>,
}

impl ModuleRules {
    pub(crate) fn load(content: &str, names: &mut NameResolver<'_>) -> Result<Self, DataError> {
        let file: ModulesFile = yaml::parse(DataFile::Modules, content)?;
        let mut variator = Variator::default();

        add_alternatives(&mut variator, &file.alternatives, names)?;
        add_from_meta(&mut variator, &file.from_meta, names)?;
        add_accept_t1(&mut variator, &file.accept_t1, names)?;
        add_from_attribute(&mut variator, &file.from_attribute, names)?;
        for name in &file.cargo_ignore {
            if let Some(id) = names.id(name, || format!("{FILE}: cargo_ignore")) {
                variator.cargo_ignore.insert(id);
            }
        }

        let mut identification = BTreeSet::new();
        for name in &file.identification {
            let Some(id) = names.id(name, || format!("{FILE}: identification")) else {
                continue;
            };
            match variator.get(id) {
                Some(variations) => identification.extend(variations.iter().map(|v| v.to)),
                None => {
                    identification.insert(id);
                }
            }
        }

        Ok(Self {
            identification,
            drug_overrides: load_drug_overrides(&file.drugs_approve_override, names),
            fit_variations: load_fit_variations(&file.fit_variation_rules, names)?,
            srp_modified: load_srp_modified(&file.srp_modified, names)?,
            variator,
        })
    }
}

fn add_alternatives(
    variator: &mut Variator,
    groups: &[Vec<Vec<String>>],
    names: &mut NameResolver<'_>,
) -> Result<(), DataError> {
    let mut all_tiers = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        let mut tiers = BTreeMap::new();
        for (tier, modules) in (1..).zip(group) {
            for module in modules {
                if let Some(id) =
                    names.id(module, || format!("{FILE}: alternatives[{group_index}]"))
                {
                    tiers.insert(id, tier);
                }
            }
        }
        all_tiers.push(tiers);
    }
    for tiers in &all_tiers {
        variator.merge_tiers(tiers, names)?;
    }
    Ok(())
}

fn add_from_meta(
    variator: &mut Variator,
    entries: &[FromMetaEntry],
    names: &mut NameResolver<'_>,
) -> Result<(), DataError> {
    let mut all_tiers = Vec::new();
    for entry in entries {
        let context = || format!("{FILE}: from_meta ({})", entry.base);
        let Some(base) = names.id(&entry.base, context) else {
            continue;
        };
        let mut tiers = names.eve().type_variations(base);
        for extra in [&entry.abyssal, &entry.alternative].into_iter().flatten() {
            if let Some(id) = names.id(extra, context) {
                tiers.insert(id, ABYSSAL_META_LEVEL);
            }
        }
        all_tiers.push(tiers);
    }
    for tiers in &all_tiers {
        variator.merge_tiers(tiers, names)?;
    }
    Ok(())
}

fn add_accept_t1(
    variator: &mut Variator,
    entries: &[String],
    names: &mut NameResolver<'_>,
) -> Result<(), DataError> {
    let mut all_tiers = Vec::new();
    for entry in entries {
        // "Heavy Hull Maintenance Bot II" also accepts "Heavy Hull Maintenance Bot I".
        let mut chars = entry.chars();
        chars.next_back();
        let tech_1 = chars.as_str();
        let context = || format!("{FILE}: accept_t1 ({entry})");
        let tech_2 = names.id(entry, context);
        let tech_1 = names.id(tech_1, context);
        if let (Some(tech_2), Some(tech_1)) = (tech_2, tech_1) {
            all_tiers.push(BTreeMap::from([(tech_2, 2), (tech_1, 1)]));
        }
    }
    for tiers in &all_tiers {
        variator.merge_tiers(tiers, names)?;
    }
    Ok(())
}

fn add_from_attribute(
    variator: &mut Variator,
    entries: &[FromAttributeEntry],
    names: &mut NameResolver<'_>,
) -> Result<(), DataError> {
    let eve = names.eve();
    let mut all_tiers = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let attribute = AttributeId(entry.attribute);
        let mut module_ids = BTreeSet::new();
        for base in &entry.base {
            if let Some(id) = names.id(base, || format!("{FILE}: from_attribute[{index}]")) {
                module_ids.extend(eve.type_variations(id).into_keys());
            }
        }
        if module_ids.is_empty() {
            continue;
        }

        let mut values = Vec::with_capacity(module_ids.len());
        for id in module_ids {
            let value = eve.get(id).and_then(|ty| ty.attribute(attribute)).ok_or_else(|| {
                DataError::invalid(
                    FILE,
                    format!(
                        "from_attribute[{index}]: {} ({id}) has no attribute {} in the static data",
                        eve.name_of(id).unwrap_or("?"),
                        entry.attribute
                    ),
                )
            })?;
            values.push((id, value as f32));
        }
        values.sort_by(|a, b| a.1.total_cmp(&b.1));
        if entry.reverse == Some(true) {
            values.reverse();
        }

        let mut tiers = BTreeMap::new();
        let mut tier = 1;
        let mut last_value = values[0].1;
        for (id, value) in values {
            if (last_value - value).abs() > ATTRIBUTE_TIER_EPSILON {
                tier += 1;
                last_value = value;
            }
            tiers.insert(id, tier);
        }
        all_tiers.push(tiers);
    }
    for tiers in &all_tiers {
        variator.merge_tiers(tiers, names)?;
    }
    Ok(())
}

fn load_drug_overrides(
    entries: &[DrugOverrideEntry],
    names: &mut NameResolver<'_>,
) -> BTreeMap<TypeId, DrugOverride> {
    let mut overrides = BTreeMap::new();
    for entry in entries {
        let context = || format!("{FILE}: drugs_approve_override ({})", entry.detect);
        let mut rule = DrugOverride::default();
        for name in &entry.remove {
            if let Some(id) = names.id(name, context) {
                rule.remove.insert(id);
            }
        }
        for add in &entry.add {
            if let Some(id) = names.id(&add.name, context) {
                rule.add.insert(id, add.amount);
            }
        }
        if let Some(detect) = names.id(&entry.detect, context) {
            overrides.insert(detect, rule);
        }
    }
    overrides
}

fn load_fit_variations(
    entries: &[FitVariationEntry],
    names: &mut NameResolver<'_>,
) -> Result<BTreeMap<TypeId, Vec<FitVariation>>, DataError> {
    let mut by_hull: BTreeMap<TypeId, Vec<FitVariation>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let context = || format!("{FILE}: fit_variation_rules[{index}] ({})", entry.hull);
        let mut variation = FitVariation::default();
        for (list, target) in [
            (&entry.missing, &mut variation.missing),
            (&entry.extra, &mut variation.extra),
        ] {
            for module in list.iter().flatten() {
                let compare = CountCompare::parse(module.compare.as_deref()).ok_or_else(|| {
                    DataError::invalid(
                        FILE,
                        format!(
                            "fit_variation_rules[{index}]: compare {:?} is invalid; use ==, <=, >=, < or >",
                            module.compare.as_deref().unwrap_or_default()
                        ),
                    )
                })?;
                if let Some(id) = names.id(&module.name, context) {
                    target.insert(
                        id,
                        VariationCount {
                            amount: module.amount,
                            compare,
                        },
                    );
                }
            }
        }
        if let Some(hull) = names.id(&entry.hull, context) {
            by_hull.entry(hull).or_default().push(variation);
        }
    }
    Ok(by_hull)
}

fn load_srp_modified(
    entries: &[SrpModifiedEntry],
    names: &mut NameResolver<'_>,
) -> Result<BTreeMap<TypeId, SrpModifiedRule>, DataError> {
    let mut rules = BTreeMap::new();
    for entry in entries {
        let pricing = match (entry.market_percent, entry.flat) {
            (Some(percent), None) => SrpModifiedPricing::MarketPercent(percent),
            (None, Some(flat)) => SrpModifiedPricing::Flat(flat),
            _ => {
                return Err(DataError::invalid(
                    FILE,
                    format!(
                        "srp_modified entry {:?} must set exactly one of market_percent or flat",
                        entry.name
                    ),
                ));
            }
        };
        let context = || format!("{FILE}: srp_modified ({})", entry.name);
        let mut resolve_all = |list: &[String]| -> BTreeSet<TypeId> {
            list.iter()
                .filter_map(|name| names.id(name, context))
                .collect()
        };
        let hulls = resolve_all(&entry.hulls);
        let exclude_hulls = resolve_all(&entry.exclude_hulls);
        if let Some(id) = names.id(&entry.name, context) {
            rules.insert(
                id,
                SrpModifiedRule {
                    pricing,
                    hulls,
                    exclude_hulls,
                },
            );
        }
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_compare_parsing() {
        assert_eq!(CountCompare::parse(None), Some(CountCompare::Eq));
        assert_eq!(CountCompare::parse(Some(" <= ")), Some(CountCompare::Lte));
        assert_eq!(CountCompare::parse(Some("GTE")), Some(CountCompare::Gte));
        assert_eq!(CountCompare::parse(Some("max")), Some(CountCompare::Lte));
        assert_eq!(CountCompare::parse(Some("~")), None);
    }

    #[test]
    fn variation_count_semantics() {
        let exact = VariationCount {
            amount: 1,
            compare: CountCompare::Eq,
        };
        assert!(exact.matches(Some(&1)));
        assert!(!exact.matches(Some(&2)));
        assert!(!exact.matches(None));

        let up_to = VariationCount {
            amount: 5,
            compare: CountCompare::Lte,
        };
        assert!(up_to.matches(None));
        assert!(up_to.matches(Some(&5)));
        assert!(!up_to.matches(Some(&6)));
    }

    #[test]
    fn srp_rule_hull_scoping() {
        let rule = SrpModifiedRule {
            pricing: SrpModifiedPricing::Flat(1),
            hulls: BTreeSet::from([1]),
            exclude_hulls: BTreeSet::new(),
        };
        assert!(rule.applies_to_hull(Some(1)));
        assert!(!rule.applies_to_hull(Some(2)));
        assert!(!rule.applies_to_hull(None));

        let unrestricted = SrpModifiedRule {
            hulls: BTreeSet::new(),
            exclude_hulls: BTreeSet::from([3]),
            ..rule
        };
        assert!(unrestricted.applies_to_hull(Some(2)));
        assert!(!unrestricted.applies_to_hull(Some(3)));
        assert!(!unrestricted.applies_to_hull(None));
    }
}
