//! Finding the doctrine fit a fit was meant to be.

use std::collections::BTreeSet;

use wl_eve_data::TypeId;

use crate::{
    DoctrineFit, DoctrineFits, Fitting,
    diff::{DiffResult, diff},
    modules::ModuleRules,
};

/// Score weights: a missing module weighs most, then an extra one, a downgrade, an upgrade.
const MISSING_WEIGHT: i64 = 12;
const EXTRA_WEIGHT: i64 = 8;
const DOWNGRADED_WEIGHT: i64 = 5;
const UPGRADED_WEIGHT: i64 = 1;
/// Differences in identifying modules (`identification` in modules.yaml) weigh this much more.
const IDENTIFYING_MULTIPLIER: i64 = 100;

/// The doctrine fit for the same hull that differs least from `fit`, with the differences. On a
/// tie the fit listed first in `fits.dat` wins.
pub fn find_fit<'a>(
    fit: &Fitting,
    fits: &'a DoctrineFits,
    rules: &ModuleRules,
) -> Option<(&'a DoctrineFit, DiffResult)> {
    fits.for_hull(fit.hull)
        .iter()
        .map(|doctrine| (doctrine, diff(&doctrine.fit, fit, rules)))
        .min_by_key(|(_, diff)| score(diff, &rules.identification))
}

fn score(diff: &DiffResult, identification: &BTreeSet<TypeId>) -> i64 {
    let multiplier = |type_id: &TypeId| {
        if identification.contains(type_id) {
            IDENTIFYING_MULTIPLIER
        } else {
            1
        }
    };
    let flat = |items: &std::collections::BTreeMap<TypeId, i64>, weight: i64| -> i64 {
        items
            .iter()
            .map(|(type_id, count)| weight * count * multiplier(type_id))
            .sum()
    };
    let nested = |items: &crate::diff::Replacements, weight: i64| -> i64 {
        items
            .iter()
            .flat_map(|(type_id, to)| to.values().map(move |count| (type_id, count)))
            .map(|(type_id, count)| weight * count * multiplier(type_id))
            .sum()
    };

    flat(&diff.module_missing, MISSING_WEIGHT)
        + flat(&diff.module_extra, EXTRA_WEIGHT)
        + nested(&diff.module_downgraded, DOWNGRADED_WEIGHT)
        + nested(&diff.module_upgraded, UPGRADED_WEIGHT)
}
