//! `skills.yaml`: skill categories and per-hull skill tiers, plus the set of skills the waitlist
//! cares about.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wl_eve_data::{EveData, NameResolver, SkillLevel, TypeId};

use crate::{DataError, DataFile, DoctrineFits, yaml};

const FILE: &str = "skills.yaml";

/// Level `gold` defaults to when not given.
const DEFAULT_GOLD_LEVEL: SkillLevel = 5;
/// Plan priority when not given.
const DEFAULT_PRIORITY: i8 = 1;

#[derive(Deserialize)]
struct SkillFile {
    categories: BTreeMap<String, Vec<String>>,
    /// `{skill set: {skill name: {min|elite|gold|priority: level}}}`.
    requirements: BTreeMap<String, BTreeMap<String, BTreeMap<String, SkillLevel>>>,
}

/// A named skill tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillTier {
    Min,
    Elite,
    Gold,
}

impl SkillTier {
    /// Parses `min`, `elite` or `gold`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "min" => Some(Self::Min),
            "elite" => Some(Self::Elite),
            "gold" => Some(Self::Gold),
            _ => None,
        }
    }
}

/// Required levels of one skill for one skill set. Serialized as in `GET /api/skills`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SkillTiers {
    /// Minimum level to fly the hull.
    #[schema(value_type = Option<i8>)]
    pub min: Option<SkillLevel>,
    /// Defaults to `min`.
    #[schema(value_type = Option<i8>)]
    pub elite: Option<SkillLevel>,
    /// Defaults to 5.
    #[schema(value_type = Option<i8>)]
    pub gold: Option<SkillLevel>,
    /// Weight in skill plan ordering; defaults to 1.
    pub priority: i8,
}

impl SkillTiers {
    pub fn get(&self, tier: SkillTier) -> Option<SkillLevel> {
        match tier {
            SkillTier::Min => self.min,
            SkillTier::Elite => self.elite,
            SkillTier::Gold => self.gold,
        }
    }
}

/// Requirements per skill set (usually a hull name, or a variant such as "Armor Kronos"; see
/// [`is_skill_group`] for the shared groups).
pub type SkillRequirements = BTreeMap<String, BTreeMap<TypeId, SkillTiers>>;

/// Whether a `requirements` key of `skills.yaml` is a shared skill group rather than a hull (or
/// hull variant): a block such as `_generic` or `_armor` that the file merges into hulls with
/// YAML anchors (`<<: *armor`). Groups are named with a leading `_`; they are loaded, and served
/// by `GET /api/skills`, like any other skill set, as in the legacy waitlist.
pub fn is_skill_group(skill_set: &str) -> bool {
    skill_set.starts_with('_')
}
/// Skill category name → skills.
pub type SkillCategories = BTreeMap<String, Vec<TypeId>>;

/// Everything derived from `skills.yaml`.
#[derive(Debug, Clone, Default)]
pub struct SkillData {
    pub requirements: SkillRequirements,
    pub categories: SkillCategories,
    /// Skills in `skills.yaml`, skills needed by any doctrine fit, and all their prerequisites.
    pub relevant_skills: BTreeSet<TypeId>,
    /// Skill name → id, for the relevant skills.
    pub name_lookup: BTreeMap<String, TypeId>,
    /// Skill id → name, for the relevant skills.
    pub id_lookup: BTreeMap<TypeId, String>,
}

impl SkillData {
    pub(crate) fn load(
        content: &str,
        fits: &DoctrineFits,
        names: &mut NameResolver<'_>,
    ) -> Result<Self, DataError> {
        let file: SkillFile = yaml::parse(DataFile::Skills, content)?;

        let mut categories = BTreeMap::new();
        for (category, skill_names) in file.categories {
            let skills = skill_names
                .iter()
                .filter_map(|name| names.id(name, || format!("{FILE}: categories.{category}")))
                .collect();
            categories.insert(category, skills);
        }

        let mut requirements = BTreeMap::new();
        let mut relevant_skills = BTreeSet::new();
        for (skill_set, skills) in file.requirements {
            let mut these = BTreeMap::new();
            for (skill_name, tiers) in skills {
                let Some(skill_id) =
                    names.id(&skill_name, || format!("{FILE}: requirements.{skill_set}"))
                else {
                    continue;
                };
                let min = tiers.get("min").copied();
                these.insert(
                    skill_id,
                    SkillTiers {
                        min,
                        elite: tiers.get("elite").copied().or(min),
                        gold: tiers.get("gold").copied().or(Some(DEFAULT_GOLD_LEVEL)),
                        priority: tiers.get("priority").copied().unwrap_or(DEFAULT_PRIORITY),
                    },
                );
                relevant_skills.insert(skill_id);
            }
            requirements.insert(skill_set, these);
        }

        let eve = names.eve();
        extend_with_prerequisites(&mut relevant_skills, fits, eve);
        let id_lookup = eve.names_of(relevant_skills.iter().copied());
        let name_lookup = id_lookup
            .iter()
            .map(|(&id, name)| (name.clone(), id))
            .collect();

        Ok(Self {
            requirements,
            categories,
            relevant_skills,
            name_lookup,
            id_lookup,
        })
    }
}

/// Adds the skills needed by every doctrine fit, then every prerequisite of every known skill.
fn extend_with_prerequisites(skills: &mut BTreeSet<TypeId>, fits: &DoctrineFits, eve: &EveData) {
    for type_id in fits.used_type_ids() {
        if let Some(ty) = eve.get(type_id) {
            skills.extend(ty.skill_requirements.keys().copied());
        }
    }

    let mut to_process: Vec<TypeId> = skills.iter().copied().collect();
    while let Some(skill_id) = to_process.pop() {
        if let Some(skill) = eve.get(skill_id) {
            for &requirement in skill.skill_requirements.keys() {
                if skills.insert(requirement) {
                    to_process.push(requirement);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_skill_group;

    #[test]
    fn groups_have_a_leading_underscore() {
        assert!(is_skill_group("_generic"));
        assert!(is_skill_group("_rigging"));
        assert!(!is_skill_group("Vindicator"));
        assert!(!is_skill_group("Armor Kronos"));
    }
}
