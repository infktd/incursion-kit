//! `rules.yaml`: community-specific rules of the fit checker, x-up and skill plans.
//!
//! The legacy waitlist had these hardcoded; here they are data, with a built-in default
//! ([`crate::DEFAULT_RULES`]) that reproduces the legacy behaviour.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use wl_eve_data::{NameResolver, SkillLevel, TypeId};

use crate::{DataError, DataFile, yaml};

const FILE: &str = "rules.yaml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    fit_check: FitCheckFile,
    fittings: FittingsRules,
    war: WarRules,
    skill_plans: SkillPlansFile,
    /// Optional so that a `rules.yaml` copied before this section existed keeps working: the
    /// built-in section applies then.
    #[serde(default)]
    fleet: Option<FleetRulesFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FleetRulesFile {
    motd_roles: Vec<MotdRoleFile>,
    motd_link_type: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MotdRoleFile {
    name: String,
    #[serde(default)]
    hulls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FitCheckFile {
    default_category: String,
    #[serde(default)]
    access_tags: Vec<AccessTag>,
    #[serde(default)]
    badge_tags: Vec<String>,
    #[serde(default)]
    badge_auto_approve: Vec<BadgeApprovalFile>,
    #[serde(default)]
    implant_sets: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    implant_module_rules: Vec<ImplantModuleRuleFile>,
    #[serde(default)]
    time_in_fleet_limits: Vec<TimeInFleetLimitFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BadgeApprovalFile {
    hull: String,
    badge: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImplantModuleRuleFile {
    implant_set: String,
    has_set: bool,
    module: String,
    tag: String,
    #[serde(default)]
    exempt_hulls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeInFleetLimitFile {
    hull: String,
    max_hours: i64,
    #[serde(default)]
    unless_tag: Option<String>,
    tag: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillPlansFile {
    generic_skill_set: String,
    #[serde(default)]
    variant_prefixes: Vec<String>,
    #[serde(default)]
    hull_variants: Vec<HullVariants>,
    tank_levels: Vec<TankLevelFile>,
    default_tank_level: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TankLevelFile {
    name: String,
    skills: BTreeMap<String, SkillLevel>,
}

/// An access key that tags the account's fits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTag {
    pub access: String,
    pub tag: String,
}

/// A hull approved without a doctrine check for pilots holding a badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadgeApproval {
    pub hull: TypeId,
    pub badge: String,
}

/// Fitting `module` while having (or lacking) an implant set adds `tag` and blocks approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplantModuleRule {
    pub implant_set: String,
    pub has_set: bool,
    pub module: TypeId,
    pub tag: String,
    pub exempt_hulls: BTreeSet<TypeId>,
}

/// Too many hours in fleet on a hull adds `tag` and blocks approval, unless the fit has
/// `unless_tag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeInFleetLimit {
    pub hull: TypeId,
    pub max_hours: i64,
    pub unless_tag: Option<String>,
    pub tag: String,
}

/// Fit checker rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FitCheckRules {
    pub default_category: String,
    pub access_tags: Vec<AccessTag>,
    pub badge_tags: Vec<String>,
    pub badge_auto_approve: Vec<BadgeApproval>,
    pub implant_sets: BTreeMap<String, Vec<TypeId>>,
    pub implant_module_rules: Vec<ImplantModuleRule>,
    pub time_in_fleet_limits: Vec<TimeInFleetLimit>,
}

/// Rules for the public fittings list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FittingsRules {
    /// Doctrine fits whose name contains this are not listed.
    pub hidden_fit_marker: String,
    /// The category whose rule items are sent as `rules`.
    pub rules_category: String,
}

/// X-up restrictions for pilots whose corporation or alliance is at war.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WarRules {
    /// Category ids that cannot be x'd up while at war.
    pub blocked_categories: Vec<String>,
    /// Tag added to the fits of pilots at war.
    pub tag: String,
}

/// Picks the skill set of a doctrine fit by its name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FitNameVariant {
    pub fit_name_contains: String,
    pub skill_set: String,
}

/// A hull with several skill sets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HullVariants {
    /// Hull name, as written in skill plans.
    pub hull: String,
    /// Skill set of fit plans whose fit name matches no variant.
    pub default: String,
    pub by_fit_name: Vec<FitNameVariant>,
}

impl HullVariants {
    /// The skill set whose `fit_name_contains` matches, if any.
    pub fn matching(&self, fit_name: &str) -> Option<&str> {
        self.by_fit_name
            .iter()
            .find(|variant| fit_name.contains(&variant.fit_name_contains))
            .map(|variant| variant.skill_set.as_str())
    }
}

/// Skills of one `type: tank` plan level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TankLevel {
    pub name: String,
    pub skills: BTreeSet<(TypeId, SkillLevel)>,
}

/// Skill plan rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPlanRules {
    pub generic_skill_set: String,
    pub variant_prefixes: Vec<String>,
    pub hull_variants: Vec<HullVariants>,
    pub tank_levels: Vec<TankLevel>,
    pub default_tank_level: String,
}

impl SkillPlanRules {
    /// Variants of a hull, if it has several skill sets.
    pub fn hull_variants(&self, hull: &str) -> Option<&HullVariants> {
        self.hull_variants
            .iter()
            .find(|variants| variants.hull == hull)
    }

    /// The skill set a fit plan for this hull and fit uses.
    pub fn skill_set_for_fit<'a>(&'a self, hull: &'a str, fit_name: &str) -> &'a str {
        match self.hull_variants(hull) {
            Some(variants) => variants.matching(fit_name).unwrap_or(&variants.default),
            None => hull,
        }
    }

    /// Whether a skill set name looks like a tank variant ("Armor Kronos").
    pub fn is_variant_name(&self, name: &str) -> bool {
        self.variant_prefixes
            .iter()
            .any(|prefix| name.contains(prefix.as_str()))
    }

    /// The hull name of a variant name ("Armor Kronos" → "Kronos"); the name itself otherwise.
    pub fn base_hull<'a>(&self, name: &'a str) -> &'a str {
        self.variant_prefixes
            .iter()
            .find_map(|prefix| {
                name.strip_prefix(prefix.as_str())
                    .and_then(|rest| rest.strip_prefix(' '))
            })
            .unwrap_or(name)
    }

    /// Whether `name` is `hull` or one of its variant names.
    pub fn is_hull_or_variant(&self, name: &str, hull: &str) -> bool {
        name == hull
            || self
                .variant_prefixes
                .iter()
                .any(|prefix| name == format!("{prefix} {hull}"))
    }

    /// The skills of a tank level; unknown names use the default level.
    pub fn tank_level(&self, name: &str) -> Option<&TankLevel> {
        self.tank_levels
            .iter()
            .find(|level| level.name == name)
            .or_else(|| {
                self.tank_levels
                    .iter()
                    .find(|level| level.name == self.default_tank_level)
            })
    }

    /// Whether a tank level name is listed.
    pub fn is_tank_level(&self, name: &str) -> bool {
        self.tank_levels.iter().any(|level| level.name == name)
    }

    /// Tank level names as an error message lists them: `'a', 'b', or 'c'`.
    pub fn tank_level_list(&self) -> String {
        let quoted: Vec<String> = self
            .tank_levels
            .iter()
            .map(|level| format!("'{}'", level.name))
            .collect();
        match quoted.split_last() {
            None => String::new(),
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
        }
    }
}

/// A fleet role assigned in the fleet MOTD (`<NAME>: <pilot links>`) and the hulls it flies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotdRole {
    /// The label as written in the MOTD (matched case-insensitively) and as the API reports it.
    pub name: String,
    pub hulls: BTreeSet<TypeId>,
}

/// Fleet rules: MOTD role assignments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRules {
    pub motd_roles: Vec<MotdRole>,
    /// `showinfo:<type>//<id>` link type for pilots added to the MOTD when the section has no
    /// link to copy it from (1377 = Character).
    pub motd_link_type: i64,
}

impl FleetRules {
    /// The role named `name`, case-insensitively.
    pub fn role(&self, name: &str) -> Option<&MotdRole> {
        self.motd_roles
            .iter()
            .find(|role| role.name.eq_ignore_ascii_case(name))
    }

    /// Every role name, in file order.
    pub fn role_names(&self) -> Vec<&str> {
        self.motd_roles
            .iter()
            .map(|role| role.name.as_str())
            .collect()
    }
}

/// Every community rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunityRules {
    pub fit_check: FitCheckRules,
    pub fittings: FittingsRules,
    pub war: WarRules,
    pub skill_plans: SkillPlanRules,
    pub fleet: FleetRules,
}

impl CommunityRules {
    pub(crate) fn load(content: &str, names: &mut NameResolver<'_>) -> Result<Self, DataError> {
        let file: RulesFile = yaml::parse(DataFile::Rules, content)?;
        let fit_check = load_fit_check(file.fit_check, names)?;
        let skill_plans = load_skill_plans(file.skill_plans, names)?;
        let fleet = match file.fleet {
            Some(fleet) => fleet,
            None => yaml::parse::<RulesFile>(DataFile::Rules, crate::DEFAULT_RULES)?
                .fleet
                .ok_or_else(|| DataError::invalid(FILE, "built-in rules lack `fleet`"))?,
        };
        let fleet = load_fleet(fleet, names)?;
        Ok(Self {
            fit_check,
            fittings: file.fittings,
            war: file.war,
            skill_plans,
            fleet,
        })
    }
}

fn load_fleet(file: FleetRulesFile, names: &mut NameResolver<'_>) -> Result<FleetRules, DataError> {
    let mut motd_roles: Vec<MotdRole> = Vec::new();
    for role in file.motd_roles {
        let name = role.name.trim().to_string();
        if name.is_empty() || name.contains(|c: char| c == ':' || c.is_whitespace()) {
            return Err(DataError::invalid(
                FILE,
                format!("fleet.motd_roles: invalid role name {:?}", role.name),
            ));
        }
        if motd_roles
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&name))
        {
            return Err(DataError::invalid(
                FILE,
                format!("fleet.motd_roles: role {name:?} is listed twice"),
            ));
        }
        let hulls = resolve_all(names, &role.hulls, &format!("fleet.motd_roles.{name}"));
        motd_roles.push(MotdRole { name, hulls });
    }
    Ok(FleetRules {
        motd_roles,
        motd_link_type: file.motd_link_type,
    })
}

fn resolve_all(names: &mut NameResolver<'_>, list: &[String], context: &str) -> BTreeSet<TypeId> {
    list.iter()
        .filter_map(|name| names.id(name, || format!("{FILE}: {context}")))
        .collect()
}

fn load_fit_check(
    file: FitCheckFile,
    names: &mut NameResolver<'_>,
) -> Result<FitCheckRules, DataError> {
    let badge_auto_approve = file
        .badge_auto_approve
        .into_iter()
        .filter_map(|entry| {
            let hull = names.id(&entry.hull, || {
                format!("{FILE}: fit_check.badge_auto_approve")
            })?;
            Some(BadgeApproval {
                hull,
                badge: entry.badge,
            })
        })
        .collect();

    let mut implant_sets = BTreeMap::new();
    for (set, implants) in file.implant_sets {
        let ids = implants
            .iter()
            .filter_map(|name| names.id(name, || format!("{FILE}: fit_check.implant_sets.{set}")))
            .collect();
        implant_sets.insert(set, ids);
    }

    let mut implant_module_rules = Vec::new();
    for rule in file.implant_module_rules {
        if !implant_sets.contains_key(&rule.implant_set) {
            return Err(DataError::invalid(
                FILE,
                format!(
                    "fit_check.implant_module_rules: implant set {:?} is not defined in implant_sets",
                    rule.implant_set
                ),
            ));
        }
        let exempt_hulls = resolve_all(
            names,
            &rule.exempt_hulls,
            "fit_check.implant_module_rules.exempt_hulls",
        );
        if let Some(module) = names.id(&rule.module, || {
            format!("{FILE}: fit_check.implant_module_rules")
        }) {
            implant_module_rules.push(ImplantModuleRule {
                implant_set: rule.implant_set,
                has_set: rule.has_set,
                module,
                tag: rule.tag,
                exempt_hulls,
            });
        }
    }

    let time_in_fleet_limits = file
        .time_in_fleet_limits
        .into_iter()
        .filter_map(|limit| {
            let hull = names.id(&limit.hull, || {
                format!("{FILE}: fit_check.time_in_fleet_limits")
            })?;
            Some(TimeInFleetLimit {
                hull,
                max_hours: limit.max_hours,
                unless_tag: limit.unless_tag,
                tag: limit.tag,
            })
        })
        .collect();

    Ok(FitCheckRules {
        default_category: file.default_category,
        access_tags: file.access_tags,
        badge_tags: file.badge_tags,
        badge_auto_approve,
        implant_sets,
        implant_module_rules,
        time_in_fleet_limits,
    })
}

fn load_skill_plans(
    file: SkillPlansFile,
    names: &mut NameResolver<'_>,
) -> Result<SkillPlanRules, DataError> {
    for variants in &file.hull_variants {
        names.id(&variants.hull, || {
            format!("{FILE}: skill_plans.hull_variants")
        });
    }

    let mut tank_levels = Vec::new();
    for level in file.tank_levels {
        let skills = level
            .skills
            .iter()
            .filter_map(|(name, &skill_level)| {
                let id = names.id(name, || {
                    format!("{FILE}: skill_plans.tank_levels.{}", level.name)
                })?;
                Some((id, skill_level))
            })
            .collect();
        tank_levels.push(TankLevel {
            name: level.name,
            skills,
        });
    }
    if !tank_levels
        .iter()
        .any(|level| level.name == file.default_tank_level)
    {
        return Err(DataError::invalid(
            FILE,
            format!(
                "skill_plans.default_tank_level {:?} is not one of tank_levels",
                file.default_tank_level
            ),
        ));
    }

    Ok(SkillPlanRules {
        generic_skill_set: file.generic_skill_set,
        variant_prefixes: file.variant_prefixes,
        hull_variants: file.hull_variants,
        tank_levels,
        default_tank_level: file.default_tank_level,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_rules() -> SkillPlanRules {
        SkillPlanRules {
            generic_skill_set: "Megathron".into(),
            variant_prefixes: vec!["Armor".into(), "Shield".into()],
            hull_variants: vec![HullVariants {
                hull: "Kronos".into(),
                default: "Armor Kronos".into(),
                by_fit_name: vec![
                    FitNameVariant {
                        fit_name_contains: "ARMOR".into(),
                        skill_set: "Armor Kronos".into(),
                    },
                    FitNameVariant {
                        fit_name_contains: "SHIELD".into(),
                        skill_set: "Shield Kronos".into(),
                    },
                ],
            }],
            tank_levels: ["starter", "min", "elite", "gold", "bastion"]
                .into_iter()
                .map(|name| TankLevel {
                    name: name.into(),
                    skills: BTreeSet::new(),
                })
                .collect(),
            default_tank_level: "min".into(),
        }
    }

    #[test]
    fn skill_set_selection() {
        let rules = plan_rules();
        assert_eq!(
            rules.skill_set_for_fit("Kronos", "X_SHIELD_KRONOS"),
            "Shield Kronos"
        );
        assert_eq!(
            rules.skill_set_for_fit("Kronos", "X_ARMOR_KRONOS"),
            "Armor Kronos"
        );
        assert_eq!(
            rules.skill_set_for_fit("Kronos", "X_KRONOS"),
            "Armor Kronos"
        );
        assert_eq!(rules.skill_set_for_fit("Paladin", "X_SHIELD"), "Paladin");
    }

    #[test]
    fn variant_names() {
        let rules = plan_rules();
        assert!(rules.is_variant_name("Armor Kronos"));
        assert!(rules.is_variant_name("Armored Thing"));
        assert!(!rules.is_variant_name("Kronos"));
        assert_eq!(rules.base_hull("Shield Kronos"), "Kronos");
        assert_eq!(rules.base_hull("Armored Thing"), "Armored Thing");
        assert!(rules.is_hull_or_variant("Armor Kronos", "Kronos"));
        assert!(!rules.is_hull_or_variant("Armor Paladin", "Kronos"));
    }

    #[test]
    fn tank_levels() {
        let rules = plan_rules();
        assert_eq!(rules.tank_level("bastion").unwrap().name, "bastion");
        assert_eq!(rules.tank_level("whatever").unwrap().name, "min");
        assert_eq!(
            rules.tank_level_list(),
            "'starter', 'min', 'elite', 'gold', or 'bastion'"
        );
    }
}
