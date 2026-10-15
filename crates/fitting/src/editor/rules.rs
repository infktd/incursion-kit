//! Community rules (`rules.yaml`), edited as one typed document.

use std::collections::BTreeSet;

use wl_eve_data::EveData;

use super::{Edited, Problems, Rejection, check_name, resolve, write_yaml};
use crate::{DEFAULT_RULES, DataError, DataFile, yaml};

pub use crate::rules::{
    BadgeApprovalEntry, FitCheckSection, FleetSection, ImplantModuleRuleEntry, MotdRoleEntry,
    RulesDocument, SkillPlansSection, TankLevelEntry, TimeInFleetLimitEntry,
};

/// Parses `rules.yaml`. A file without the optional `fleet` section gets the built-in one, which
/// is what applies.
pub fn read_rules(content: &str) -> Result<RulesDocument, DataError> {
    let mut document: RulesDocument = yaml::parse(DataFile::Rules, content)?;
    if document.fleet.is_none() {
        document.fleet = yaml::parse::<RulesDocument>(DataFile::Rules, DEFAULT_RULES)?.fleet;
    }
    Ok(document)
}

fn not_empty(problems: &mut Problems, path: &str, value: &str) {
    if value.trim().is_empty() {
        problems.error(path, "must not be empty");
    }
}

fn resolve_all(eve: &EveData, problems: &mut Problems, path: &str, names: &[String]) {
    for (index, name) in names.iter().enumerate() {
        resolve(eve, problems, &format!("{path}[{index}]"), name);
    }
}

fn check(document: &RulesDocument, eve: &EveData) -> Problems {
    let mut problems = Problems::default();
    let p = &mut problems;

    let fit_check = &document.fit_check;
    not_empty(p, "fit_check.default_category", &fit_check.default_category);
    for (index, entry) in fit_check.access_tags.iter().enumerate() {
        not_empty(
            p,
            &format!("fit_check.access_tags[{index}].access"),
            &entry.access,
        );
        not_empty(
            p,
            &format!("fit_check.access_tags[{index}].tag"),
            &entry.tag,
        );
    }
    for (index, badge) in fit_check.badge_tags.iter().enumerate() {
        not_empty(p, &format!("fit_check.badge_tags[{index}]"), badge);
    }
    for (index, entry) in fit_check.badge_auto_approve.iter().enumerate() {
        let path = format!("fit_check.badge_auto_approve[{index}]");
        resolve(eve, p, &format!("{path}.hull"), &entry.hull);
        not_empty(p, &format!("{path}.badge"), &entry.badge);
    }
    for (set, implants) in &fit_check.implant_sets {
        let path = format!("fit_check.implant_sets.{set}");
        if set.trim().is_empty() {
            p.error(&path, "implant set names must not be empty");
        }
        resolve_all(eve, p, &path, implants);
    }
    for (index, rule) in fit_check.implant_module_rules.iter().enumerate() {
        let path = format!("fit_check.implant_module_rules[{index}]");
        if !fit_check.implant_sets.contains_key(&rule.implant_set) {
            p.error(
                format!("{path}.implant_set"),
                format!("{:?} is not defined in implant_sets", rule.implant_set),
            );
        }
        resolve(eve, p, &format!("{path}.module"), &rule.module);
        not_empty(p, &format!("{path}.tag"), &rule.tag);
        resolve_all(eve, p, &format!("{path}.exempt_hulls"), &rule.exempt_hulls);
    }
    for (index, limit) in fit_check.time_in_fleet_limits.iter().enumerate() {
        let path = format!("fit_check.time_in_fleet_limits[{index}]");
        resolve(eve, p, &format!("{path}.hull"), &limit.hull);
        if limit.max_hours < 0 {
            p.error(format!("{path}.max_hours"), "must not be negative");
        }
        not_empty(p, &format!("{path}.tag"), &limit.tag);
    }

    // An empty marker is contained in every name and would hide every fit.
    not_empty(
        p,
        "fittings.hidden_fit_marker",
        &document.fittings.hidden_fit_marker,
    );
    not_empty(
        p,
        "fittings.rules_category",
        &document.fittings.rules_category,
    );
    not_empty(p, "war.tag", &document.war.tag);

    let plans = &document.skill_plans;
    not_empty(p, "skill_plans.generic_skill_set", &plans.generic_skill_set);
    for (index, prefix) in plans.variant_prefixes.iter().enumerate() {
        not_empty(p, &format!("skill_plans.variant_prefixes[{index}]"), prefix);
    }
    for (index, variants) in plans.hull_variants.iter().enumerate() {
        let path = format!("skill_plans.hull_variants[{index}]");
        resolve(eve, p, &format!("{path}.hull"), &variants.hull);
        not_empty(p, &format!("{path}.default"), &variants.default);
        for (variant_index, variant) in variants.by_fit_name.iter().enumerate() {
            let path = format!("{path}.by_fit_name[{variant_index}]");
            not_empty(
                p,
                &format!("{path}.fit_name_contains"),
                &variant.fit_name_contains,
            );
            not_empty(p, &format!("{path}.skill_set"), &variant.skill_set);
        }
    }
    let mut levels = BTreeSet::new();
    for (index, level) in plans.tank_levels.iter().enumerate() {
        let path = format!("skill_plans.tank_levels[{index}]");
        if check_name(p, &format!("{path}.name"), &level.name, 64)
            && !levels.insert(level.name.as_str())
        {
            p.error(
                format!("{path}.name"),
                format!("{:?} is listed twice", level.name),
            );
        }
        for (skill, &skill_level) in &level.skills {
            let path = format!("{path}.skills.{skill}");
            resolve(eve, p, &path, skill);
            if !(0..=5).contains(&skill_level) {
                p.error(path, "the level must be between 0 and 5");
            }
        }
    }
    if !levels.contains(plans.default_tank_level.as_str()) {
        p.error(
            "skill_plans.default_tank_level",
            format!("{:?} is not one of tank_levels", plans.default_tank_level),
        );
    }

    if let Some(fleet) = &document.fleet {
        let mut roles: Vec<String> = Vec::new();
        for (index, role) in fleet.motd_roles.iter().enumerate() {
            let path = format!("fleet.motd_roles[{index}]");
            let name = role.name.trim();
            if name.is_empty() || name.contains(|c: char| c == ':' || c.is_whitespace()) {
                p.error(
                    format!("{path}.name"),
                    "must not be empty or contain whitespace or `:`",
                );
            } else if roles.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                p.error(
                    format!("{path}.name"),
                    format!("role {name:?} is listed twice"),
                );
            } else {
                roles.push(name.to_string());
            }
            resolve_all(eve, p, &format!("{path}.hulls"), &role.hulls);
        }
    }
    problems
}

/// Replaces `content` (`rules.yaml`, or the built-in rules when the file is absent) with
/// `document`.
pub fn write_rules(content: &str, eve: &EveData, document: &RulesDocument) -> Edited {
    let problems = check(document, eve);
    if problems.has_errors() {
        return Err(problems.into());
    }
    let yaml = write_yaml(DataFile::Rules, content, document)
        .map_err(|error| Rejection::invalid("", error.to_string()))?;
    problems.finish(yaml)
}
