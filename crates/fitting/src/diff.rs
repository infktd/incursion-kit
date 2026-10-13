//! Comparing a fit with a doctrine fit.

use std::collections::BTreeMap;

use wl_eve_data::TypeId;

use crate::{
    Fitting,
    modules::{ModuleRules, Variation, Variator},
};

/// Expected cargo of at least this many items only counts as missing when more than
/// [`CARGO_MISSING_PERCENT`] of it is missing.
const CARGO_TOLERANCE_MIN_EXPECTED: i64 = 10;
const CARGO_MISSING_PERCENT: i64 = 80;

/// `original → replacement → count`.
pub type Replacements = BTreeMap<TypeId, BTreeMap<TypeId, i64>>;

/// How a fit differs from a doctrine fit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffResult {
    pub module_missing: BTreeMap<TypeId, i64>,
    pub module_extra: BTreeMap<TypeId, i64>,
    pub module_upgraded: Replacements,
    pub module_downgraded: Replacements,
    pub cargo_missing: BTreeMap<TypeId, i64>,
}

#[derive(Debug, Default)]
struct SectionDiff {
    missing: BTreeMap<TypeId, i64>,
    extra: BTreeMap<TypeId, i64>,
    upgraded: Replacements,
    downgraded: Replacements,
}

/// Matches expected against actual items, first allowing only equivalent replacements, then any.
fn section_diff(
    expect: &BTreeMap<TypeId, i64>,
    actual: &BTreeMap<TypeId, i64>,
    variator: &Variator,
) -> SectionDiff {
    let mut extra = actual.clone();
    let mut missing = expect.clone();
    let mut diff = SectionDiff::default();

    let variations_of = |type_id: TypeId| -> Vec<Variation> {
        match variator.get(type_id) {
            Some(variations) => variations.to_vec(),
            None => vec![Variation {
                from: type_id,
                to: type_id,
                meta_diff: 0,
            }],
        }
    };

    let mut take = |remaining: &mut i64, to: TypeId| -> i64 {
        let available = extra.get(&to).copied().unwrap_or(0);
        let taken = (*remaining).min(available);
        if taken > 0 {
            *remaining -= taken;
            if let Some(count) = extra.get_mut(&to) {
                *count -= taken;
            }
        }
        taken
    };

    // First pass: equivalent modules only.
    for (&expect_id, remaining) in missing.iter_mut() {
        for variation in variations_of(expect_id) {
            if variation.meta_diff == 0 {
                take(remaining, variation.to);
            }
        }
    }

    // Second pass: anything acceptable, recording upgrades and downgrades.
    for (&expect_id, remaining) in missing.iter_mut() {
        for variation in variations_of(expect_id) {
            let taken = take(remaining, variation.to);
            if taken <= 0 {
                continue;
            }
            let target = match variation.meta_diff {
                meta_diff if meta_diff > 0 => &mut diff.upgraded,
                meta_diff if meta_diff < 0 => &mut diff.downgraded,
                _ => continue,
            };
            *target
                .entry(variation.from)
                .or_default()
                .entry(variation.to)
                .or_insert(0) += taken;
        }
    }

    diff.missing = missing
        .into_iter()
        .filter(|&(_, count)| count > 0)
        .collect();
    diff.extra = extra.into_iter().filter(|&(_, count)| count > 0).collect();
    diff
}

/// Compares `actual` with the doctrine fit `expect`.
///
/// Cargo listed in `cargo_ignore` is not expected; booster overrides adjust the expected cargo;
/// a downgraded cargo item counts as missing; a large stack only counts as missing when more
/// than 80% of it is; and a hull's fit-variation rules forgive exact module swaps.
pub fn diff(expect: &Fitting, actual: &Fitting, rules: &ModuleRules) -> DiffResult {
    let variator = &rules.variator;
    let mut modules = section_diff(&expect.modules, &actual.modules, variator);

    let mut expected_cargo = expect.cargo.clone();
    expected_cargo.retain(|id, _| !variator.cargo_ignore.contains(id));
    for (detect, change) in &rules.drug_overrides {
        if expected_cargo.contains_key(detect) {
            expected_cargo.retain(|id, _| !change.remove.contains(id));
            for (&id, &amount) in &change.add {
                expected_cargo.insert(id, amount);
            }
        }
    }

    let cargo = section_diff(&expected_cargo, &actual.cargo, variator);
    let mut cargo_missing = cargo.missing;
    for (type_id, replacements) in cargo.downgraded {
        for count in replacements.into_values() {
            *cargo_missing.entry(type_id).or_insert(0) += count;
        }
    }
    let cargo_missing = cargo_missing
        .into_iter()
        .filter(|(type_id, count)| {
            let expected = expected_cargo.get(type_id).copied().unwrap_or(0);
            if expected >= CARGO_TOLERANCE_MIN_EXPECTED {
                *count > expected * CARGO_MISSING_PERCENT / 100
            } else {
                true
            }
        })
        .collect();

    for variation in rules.fit_variations.get(&actual.hull).into_iter().flatten() {
        let missing_matches = variation
            .missing
            .iter()
            .all(|(module, count)| count.matches(modules.missing.get(module)));
        let extra_matches = variation
            .extra
            .iter()
            .all(|(module, count)| count.matches(modules.extra.get(module)));
        if missing_matches && extra_matches {
            for module in variation.missing.keys() {
                modules.missing.remove(module);
            }
            for module in variation.extra.keys() {
                modules.extra.remove(module);
            }
        }
    }

    DiffResult {
        module_missing: modules.missing,
        module_extra: modules.extra,
        module_upgraded: modules.upgraded,
        module_downgraded: modules.downgraded,
        cargo_missing,
    }
}
