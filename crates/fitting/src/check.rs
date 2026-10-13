//! The fit checker: decides whether an x'd-up fit is auto-approved, which category it goes in,
//! which tags it gets and how it differs from its doctrine fit.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use wl_eve_data::{SkillLevel, TypeId};

use crate::{
    DataSet, FitError, Fitting, diff::Replacements, matcher::find_fit, modules::SrpModifiedPricing,
};

/// A pilot's trained skill levels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PilotSkills(pub HashMap<TypeId, SkillLevel>);

impl PilotSkills {
    /// The level of a skill; 0 when untrained.
    pub fn get(&self, skill_id: TypeId) -> SkillLevel {
        self.0.get(&skill_id).copied().unwrap_or(0)
    }
}

/// What the fit checker needs to know about the pilot.
#[derive(Debug, Clone, Copy)]
pub struct PilotData<'a> {
    /// Implants currently plugged in.
    pub implants: &'a [TypeId],
    /// Total seconds spent in fleet.
    pub time_in_fleet: i64,
    pub skills: &'a PilotSkills,
    /// The account's permission keys (`waitlist-tag:HQ-FC`...).
    pub access_keys: &'a BTreeSet<String>,
}

/// An SRP price override on a module of the fit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SrpModifiedModule {
    pub type_id: TypeId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat: Option<i64>,
}

/// How a fit differs from the doctrine fit it matched. Stored as JSON in
/// `waitlist_entry_fit.fit_analysis` and sent to the frontend verbatim, so the field order and
/// shape are part of the API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FitAnalysis {
    /// Name of the matched doctrine fit.
    pub name: String,
    pub missing: BTreeMap<TypeId, i64>,
    pub extra: BTreeMap<TypeId, i64>,
    pub cargo_missing: BTreeMap<TypeId, i64>,
    /// Original module → the module it was downgraded to → count.
    #[schema(value_type = BTreeMap<TypeId, BTreeMap<TypeId, i64>>)]
    pub downgraded: Replacements,
    /// SRP price overrides on modules of the fit or in its differences. Informational only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub srp_modified: Vec<SrpModifiedModule>,
}

/// The fit checker's verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct FitCheckOutput {
    pub approved: bool,
    /// Sorted.
    pub tags: Vec<String>,
    /// Category id.
    pub category: String,
    /// Reasons the fit cannot be x'd up at all; the x-up endpoint reports the first.
    pub errors: Vec<String>,
    /// `None` when no doctrine fit was compared.
    pub analysis: Option<FitAnalysis>,
}

impl DataSet {
    /// Checks a fit (which must already have passed [`Fitting::validate`]) for a pilot holding
    /// `badges`.
    pub fn check_fit(
        &self,
        pilot: &PilotData<'_>,
        fit: &Fitting,
        badges: &[String],
    ) -> Result<FitCheckOutput, FitError> {
        let mut checker = Checker {
            data: self,
            pilot,
            fit,
            badges,
            approved: true,
            tags: BTreeSet::new(),
            errors: Vec::new(),
            analysis: None,
        };
        checker.check_module_skills()?;
        checker.check_fit();
        let category = self
            .categories
            .categorize(fit)
            .unwrap_or(&self.rules.fit_check.default_category)
            .to_string();
        checker.add_role_and_badge_tags();
        checker.check_time_in_fleet();

        Ok(FitCheckOutput {
            approved: checker.approved,
            tags: checker.tags.into_iter().collect(),
            category,
            errors: checker.errors,
            analysis: checker.analysis,
        })
    }
}

struct Checker<'a> {
    data: &'a DataSet,
    pilot: &'a PilotData<'a>,
    fit: &'a Fitting,
    badges: &'a [String],
    approved: bool,
    tags: BTreeSet<String>,
    errors: Vec<String>,
    analysis: Option<FitAnalysis>,
}

impl Checker<'_> {
    fn has_badge(&self, badge: &str) -> bool {
        self.badges.iter().any(|held| held == badge)
    }

    /// One error per missing skill of the hull and each fitted module.
    fn check_module_skills(&mut self) -> Result<(), FitError> {
        let eve = self.data.eve();
        for type_id in std::iter::once(self.fit.hull).chain(self.fit.modules.keys().copied()) {
            let ty = eve.get(type_id).ok_or(FitError::InvalidModule)?;
            for (&skill_id, &level) in &ty.skill_requirements {
                if self.pilot.skills.get(skill_id) < level {
                    self.errors
                        .push(format!("Missing skills to online/use '{}'", ty.name));
                }
            }
        }
        Ok(())
    }

    fn check_fit(&mut self) {
        let rules = &self.data.rules.fit_check;
        if rules
            .badge_auto_approve
            .iter()
            .any(|rule| rule.hull == self.fit.hull && self.has_badge(&rule.badge))
        {
            self.approved = true;
            return;
        }

        self.check_implant_module_rules();

        let Some((doctrine, diff)) = find_fit(self.fit, &self.data.fits, &self.data.modules) else {
            self.approved = false;
            return;
        };
        let fit_ok = diff.module_downgraded.is_empty() && diff.module_missing.is_empty();
        if !(diff.cargo_missing.is_empty() && fit_ok) {
            self.approved = false;
        }
        let mut analysis = FitAnalysis {
            name: doctrine.name.clone(),
            missing: diff.module_missing,
            extra: diff.module_extra,
            cargo_missing: diff.cargo_missing,
            downgraded: diff.module_downgraded,
            srp_modified: Vec::new(),
        };
        analysis.srp_modified = self.srp_modified(&analysis);
        self.analysis = Some(analysis);
    }

    fn check_implant_module_rules(&mut self) {
        let rules = &self.data.rules.fit_check;
        for rule in &rules.implant_module_rules {
            if rule.exempt_hulls.contains(&self.fit.hull)
                || !self.fit.modules.contains_key(&rule.module)
            {
                continue;
            }
            let has_set = rules
                .implant_sets
                .get(&rule.implant_set)
                .is_some_and(|set| {
                    set.iter()
                        .all(|implant| self.pilot.implants.contains(implant))
                });
            if has_set == rule.has_set {
                self.tags.insert(rule.tag.clone());
                self.approved = false;
            }
        }
    }

    /// SRP overrides for modules on the fit or in any difference from the doctrine fit.
    fn srp_modified(&self, analysis: &FitAnalysis) -> Vec<SrpModifiedModule> {
        let overrides = &self.data.modules.srp_modified;
        if overrides.is_empty() {
            return Vec::new();
        }

        let mut candidates = BTreeSet::new();
        candidates.extend(self.fit.modules.keys().copied());
        candidates.extend(self.fit.cargo.keys().copied());
        candidates.extend(analysis.missing.keys().copied());
        candidates.extend(analysis.extra.keys().copied());
        candidates.extend(analysis.cargo_missing.keys().copied());
        for (original, replacements) in &analysis.downgraded {
            candidates.insert(*original);
            candidates.extend(replacements.keys().copied());
        }

        candidates
            .into_iter()
            .filter_map(|type_id| {
                let rule = overrides.get(&type_id)?;
                if !rule.applies_to_hull(Some(self.fit.hull)) {
                    return None;
                }
                let (market_percent, flat) = match rule.pricing {
                    SrpModifiedPricing::MarketPercent(percent) => (Some(percent), None),
                    SrpModifiedPricing::Flat(amount) => (None, Some(amount)),
                };
                Some(SrpModifiedModule {
                    type_id,
                    market_percent,
                    flat,
                })
            })
            .collect()
    }

    /// Role tags (first matching access key) or, without one, badge tags.
    fn add_role_and_badge_tags(&mut self) {
        let rules = &self.data.rules.fit_check;
        if let Some(access_tag) = rules
            .access_tags
            .iter()
            .find(|entry| self.pilot.access_keys.contains(&entry.access))
        {
            self.tags.insert(access_tag.tag.clone());
            return;
        }
        for badge in &rules.badge_tags {
            if self.has_badge(badge) {
                self.tags.insert(badge.clone());
            }
        }
    }

    fn check_time_in_fleet(&mut self) {
        let rules = &self.data.rules.fit_check;
        for limit in &rules.time_in_fleet_limits {
            if limit.hull != self.fit.hull {
                continue;
            }
            let exempt = limit
                .unless_tag
                .as_ref()
                .is_some_and(|tag| self.tags.contains(tag));
            if self.pilot.time_in_fleet > limit.max_hours * 3600 && !exempt {
                self.approved = false;
                self.tags.insert(limit.tag.clone());
            }
        }
    }
}
