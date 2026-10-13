//! `skillplan.yaml`: skill plans, turned into ordered training steps.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use serde::{Deserialize, Serialize};
use wl_core::types::Hull;
use wl_eve_data::{AttributeId, EveData, SkillLevel, TypeError, TypeId};

use crate::{DataError, DataFile, DoctrineFits, SkillData, rules::SkillPlanRules, yaml};

/// A skill plan could not be built or is invalid. Messages match the legacy waitlist, whose API
/// error bodies expose them (`skill plan error: validation error: ...`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillPlanError {
    #[error("fit not found")]
    FitNotFound,
    #[error("type error")]
    Type(#[from] TypeError),
    #[error("invalid tier")]
    InvalidTier,
    #[error("file write error: {0}")]
    FileWrite(String),
    #[error("validation error: {0}")]
    Validation(String),
    /// A plan refers to a skill set that `skills.yaml` does not define.
    #[error("unknown skill set '{0}'")]
    UnknownSkillSet(String),
    /// The static data lacks a skill's training time multiplier.
    #[error("skill {0} has no training time multiplier")]
    MissingTrainingTime(TypeId),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SkillPlanFile {
    plans: Vec<SkillPlan>,
}

/// A skill plan as written in `skillplan.yaml` and exchanged with the admin API.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
pub struct SkillPlan {
    /// Unique name; identifies the plan in the admin API.
    pub name: String,
    pub description: String,
    /// The steps, in order.
    pub plan: Vec<SkillPlanLevel>,
}

/// One step of a skill plan.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SkillPlanLevel {
    /// Skills needed to fly a doctrine fit.
    Fit {
        /// Hull name.
        hull: String,
        /// Fit name in `fits.dat`.
        fit: String,
    },
    /// A tier (`min`, `elite`, `gold`) of a skill set from `skills.yaml`.
    Skills {
        /// Skill set name (usually a hull).
        from: String,
        tier: String,
    },
    /// One skill to a level.
    Skill {
        /// Skill name.
        from: String,
        #[schema(value_type = i8)]
        level: SkillLevel,
    },
    /// Armor compensation skills of a tank level (see `rules.yaml`).
    Tank {
        /// Tank level name.
        from: String,
    },
}

/// `(skill, level)`.
pub type LevelPair = (TypeId, SkillLevel);

/// A plan as sent by `GET /api/skills/plans`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SkillPlansResponsePlan {
    pub source: SkillPlan,
    /// Training steps in order, without duplicates: `[skill id, level]` pairs.
    #[schema(value_type = Vec<(i32, i8)>)]
    pub levels: Vec<LevelPair>,
    /// Hulls (or tank variants of a hull) the plan is for.
    pub ships: Vec<Hull>,
}

/// `GET /api/skills/plans`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default, utoipa::ToSchema)]
pub struct SkillPlansResponse {
    pub plans: Vec<SkillPlansResponsePlan>,
}

/// Parsed `skillplan.yaml`, with every plan that builds.
#[derive(Debug, Clone, Default)]
pub struct SkillPlans {
    /// Plans in file order, as written.
    pub plans: Vec<SkillPlan>,
    /// The response of `GET /api/skills/plans`; plans that fail to build are left out.
    pub response: SkillPlansResponse,
}

/// Parses `skillplan.yaml`.
pub fn parse_skill_plans(content: &str) -> Result<Vec<SkillPlan>, DataError> {
    let file: SkillPlanFile = yaml::parse(DataFile::SkillPlans, content)?;
    Ok(file.plans)
}

/// Renders plans as `skillplan.yaml` (used by the skill plan admin endpoints).
pub fn skill_plans_to_yaml(plans: &[SkillPlan]) -> Result<String, DataError> {
    yaml::to_string(
        DataFile::SkillPlans,
        &SkillPlanFile {
            plans: plans.to_vec(),
        },
    )
}

/// What plan building needs.
#[derive(Clone, Copy)]
pub(crate) struct PlanContext<'a> {
    pub eve: &'a EveData,
    pub fits: &'a DoctrineFits,
    pub skills: &'a SkillData,
    pub rules: &'a SkillPlanRules,
}

impl PlanContext<'_> {
    /// Ordered, deduplicated training steps of a plan.
    pub fn build_plan(&self, plan: &SkillPlan) -> Result<Vec<LevelPair>, SkillPlanError> {
        let mut seen = BTreeSet::new();
        let mut steps = Vec::new();
        for level in &plan.plan {
            let these = match level {
                SkillPlanLevel::Fit { hull, fit } => self.fit_plan(hull, fit)?,
                SkillPlanLevel::Skills { from, tier } => self.tier_plan(from, tier)?,
                SkillPlanLevel::Skill { from, level } => self.single_skill_plan(from, *level)?,
                SkillPlanLevel::Tank { from } => self.tank_plan(from)?,
            };
            for step in these {
                if seen.insert(step) {
                    steps.push(step);
                }
            }
        }
        Ok(steps)
    }

    fn fit_plan(&self, hull: &str, fit_name: &str) -> Result<Vec<LevelPair>, SkillPlanError> {
        let hull_id = self.eve.id_of(hull).ok_or(TypeError::NothingMatched)?;
        let doctrine = self
            .fits
            .find(hull_id, fit_name)
            .ok_or(SkillPlanError::FitNotFound)?;

        let mut requirements = BTreeSet::new();
        for type_id in
            std::iter::once(doctrine.fit.hull).chain(doctrine.fit.modules.keys().copied())
        {
            let ty = self.eve.require(type_id)?;
            requirements.extend(
                ty.skill_requirements
                    .iter()
                    .map(|(&id, &level)| (id, level)),
            );
        }

        let hull_name = self.eve.require(doctrine.fit.hull)?.name.as_str();
        let skill_set = self.rules.skill_set_for_fit(hull_name, fit_name);
        self.sorted_plan(skill_set, &requirements)
    }

    fn tier_plan(&self, skill_set: &str, tier: &str) -> Result<Vec<LevelPair>, SkillPlanError> {
        let tier = crate::SkillTier::parse(tier).ok_or(SkillPlanError::InvalidTier)?;
        let requirements = self
            .skills
            .requirements
            .get(skill_set)
            .ok_or_else(|| SkillPlanError::UnknownSkillSet(skill_set.to_string()))?
            .iter()
            .map(|(&skill_id, tiers)| (skill_id, tiers.get(tier).unwrap_or_default()))
            .filter(|&(_, level)| level > 0)
            .collect();
        self.sorted_plan(skill_set, &requirements)
    }

    fn tank_plan(&self, level: &str) -> Result<Vec<LevelPair>, SkillPlanError> {
        let requirements = self
            .rules
            .tank_level(level)
            .map(|level| level.skills.clone())
            .unwrap_or_default();
        // Tank skill order is not hull-specific.
        self.sorted_plan(&self.rules.generic_skill_set, &requirements)
    }

    fn single_skill_plan(
        &self,
        skill: &str,
        level: SkillLevel,
    ) -> Result<Vec<LevelPair>, SkillPlanError> {
        let skill_id = self.eve.id_of(skill).ok_or(TypeError::NothingMatched)?;
        self.sorted_plan(
            &self.rules.generic_skill_set,
            &BTreeSet::from([(skill_id, level)]),
        )
    }

    /// Orders `requirements` and all their prerequisites so that prerequisites come first and,
    /// among what can be trained next, the most valuable per skill point comes first (value
    /// from the skill set's priorities).
    fn sorted_plan(
        &self,
        skill_set: &str,
        requirements: &BTreeSet<LevelPair>,
    ) -> Result<Vec<LevelPair>, SkillPlanError> {
        let priority: BTreeMap<TypeId, f64> = self
            .skills
            .requirements
            .get(skill_set)
            .ok_or_else(|| SkillPlanError::UnknownSkillSet(skill_set.to_string()))?
            .iter()
            .map(|(&skill, tiers)| (skill, f64::from(tiers.priority)))
            .collect();
        let graph = skill_graph(self.eve, requirements, &priority)?;
        Ok(flatten(graph))
    }

    /// Checks a plan before it is saved (admin endpoints).
    pub fn validate_plan(&self, plan: &SkillPlan) -> Result<(), SkillPlanError> {
        let invalid = |message: String| Err(SkillPlanError::Validation(message));
        if plan.name.trim().is_empty() {
            return invalid("Plan name cannot be empty".to_string());
        }
        let not_found = TypeError::NothingMatched;

        for (index, level) in plan.plan.iter().enumerate() {
            let step = index + 1;
            match level {
                SkillPlanLevel::Fit { hull, fit } => {
                    let Some(hull_id) = self.eve.id_of(hull) else {
                        return invalid(format!(
                            "Plan step {step}: Hull '{hull}' not found: {not_found}"
                        ));
                    };
                    let hull_fits = self.fits.for_hull(hull_id);
                    if hull_fits.is_empty() {
                        return invalid(format!(
                            "Plan step {step}: No fits found for hull '{hull}'"
                        ));
                    }
                    if !hull_fits.iter().any(|candidate| candidate.name == *fit) {
                        return invalid(format!(
                            "Plan step {step}: Fit '{fit}' not found for hull '{hull}'"
                        ));
                    }
                }
                SkillPlanLevel::Skills { from, tier } => {
                    if crate::SkillTier::parse(tier).is_none() {
                        return invalid(format!(
                            "Plan step {step}: Invalid tier '{tier}'. Must be 'min', 'elite', or 'gold'"
                        ));
                    }
                    let spaced: Vec<String> = self
                        .rules
                        .variant_prefixes
                        .iter()
                        .map(|prefix| format!("{prefix} "))
                        .collect();
                    if spaced.iter().any(|prefix| from.contains(prefix.as_str())) {
                        let base = self.rules.base_hull(from);
                        if self.eve.id_of(base).is_none() {
                            return invalid(format!(
                                "Plan step {step}: Base ship '{base}' for '{from}' not found: {not_found}"
                            ));
                        }
                    } else if self.eve.id_of(from).is_none() {
                        return invalid(format!(
                            "Plan step {step}: Ship '{from}' not found: {not_found}"
                        ));
                    }
                }
                SkillPlanLevel::Skill { from, level } => {
                    if self.eve.id_of(from).is_none() {
                        return invalid(format!(
                            "Plan step {step}: Skill '{from}' not found: {not_found}"
                        ));
                    }
                    if !(1..=5).contains(level) {
                        return invalid(format!(
                            "Plan step {step}: Skill level must be between 1 and 5, got {level}"
                        ));
                    }
                }
                SkillPlanLevel::Tank { from } => {
                    if !self.rules.is_tank_level(from) {
                        return invalid(format!(
                            "Plan step {step}: Invalid tank level '{from}'. Must be {}",
                            self.rules.tank_level_list()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Hulls a plan is for, as `GET /api/skills/plans` lists them: tank variant names first
    /// (with the id of their hull, or 0), then plain hulls not already covered by a variant.
    fn plan_ships(&self, plan: &SkillPlan) -> Vec<Hull> {
        let mut variant_names: Vec<String> = Vec::new();
        let mut hull_ids: Vec<TypeId> = Vec::new();
        let mut push_name = |name: &str| {
            if !variant_names.iter().any(|known| known == name) {
                variant_names.push(name.to_string());
            }
        };
        let mut push_id = |id: TypeId| {
            if !hull_ids.contains(&id) {
                hull_ids.push(id);
            }
        };

        for level in &plan.plan {
            match level {
                SkillPlanLevel::Skills { from, .. } => {
                    if self.rules.is_variant_name(from) {
                        push_name(from);
                    } else if let Some(id) = self.eve.id_of(from) {
                        push_id(id);
                    } else {
                        push_name(from);
                    }
                }
                SkillPlanLevel::Fit { hull, fit } => {
                    if let Some(skill_set) = self
                        .rules
                        .hull_variants(hull)
                        .and_then(|variants| variants.matching(fit))
                    {
                        push_name(skill_set);
                    }
                    if let Some(id) = self.eve.id_of(hull) {
                        push_id(id);
                    }
                }
                SkillPlanLevel::Skill { .. } | SkillPlanLevel::Tank { .. } => {}
            }
        }

        let mut ships: Vec<Hull> = variant_names
            .into_iter()
            .map(|name| Hull {
                id: self.eve.id_of(self.rules.base_hull(&name)).unwrap_or(0),
                name,
            })
            .collect();
        for id in hull_ids {
            let Some(name) = self.eve.name_of(id) else {
                continue;
            };
            if !ships
                .iter()
                .any(|ship| self.rules.is_hull_or_variant(&ship.name, name))
            {
                ships.push(Hull {
                    id,
                    name: name.to_string(),
                });
            }
        }
        ships
    }

    /// Builds every plan, skipping (and reporting) those that fail.
    pub fn build_all(&self, plans: Vec<SkillPlan>) -> (SkillPlans, Vec<String>) {
        let mut response = SkillPlansResponse::default();
        let mut warnings = Vec::new();
        for plan in &plans {
            match self.build_plan(plan) {
                Ok(levels) => response.plans.push(SkillPlansResponsePlan {
                    ships: self.plan_ships(plan),
                    levels,
                    source: plan.clone(),
                }),
                Err(error) => {
                    warnings.push(format!(
                        "skillplan.yaml: plan {:?} skipped: {error}",
                        plan.name
                    ));
                }
            }
        }
        (SkillPlans { plans, response }, warnings)
    }
}

#[derive(Debug, Default)]
struct DepEntry {
    dependees: Vec<LevelPair>,
    unsatisfied_requirements: usize,
    value: i64,
}

/// Builds the prerequisite graph of `requirements`: level N needs level N-1, and level 1 needs
/// the skill's own requirements. Each node's value is its priority per skill point plus half the
/// value of everything depending on it.
fn skill_graph(
    eve: &EveData,
    requirements: &BTreeSet<LevelPair>,
    priority: &BTreeMap<TypeId, f64>,
) -> Result<BTreeMap<LevelPair, DepEntry>, SkillPlanError> {
    let mut graph: BTreeMap<LevelPair, DepEntry> = BTreeMap::new();
    let mut to_process: Vec<LevelPair> = requirements.iter().copied().collect();
    let mut processed = BTreeSet::new();

    while let Some((skill_id, skill_level)) = to_process.pop() {
        if !processed.insert((skill_id, skill_level)) {
            continue;
        }

        let mut these = BTreeSet::new();
        if skill_level >= 2 {
            these.insert((skill_id, skill_level - 1));
        } else if skill_level == 1 {
            // Only level 1 depends on other skills; the graph would explode otherwise.
            let skill = eve.require(skill_id)?;
            these.extend(
                skill
                    .skill_requirements
                    .iter()
                    .map(|(&id, &level)| (id, level)),
            );
        }

        graph
            .entry((skill_id, skill_level))
            .or_default()
            .unsatisfied_requirements = these.len();
        for requirement in these {
            to_process.push(requirement);
            graph
                .entry(requirement)
                .or_default()
                .dependees
                .push((skill_id, skill_level));
        }
    }

    let mut memo = BTreeMap::new();
    let keys: Vec<LevelPair> = graph.keys().copied().collect();
    for pair in keys {
        let value = node_value(eve, pair, &graph, priority, &mut memo)?;
        if let Some(entry) = graph.get_mut(&pair) {
            entry.value = (value * 1_000_000_000.) as i64;
        }
    }
    Ok(graph)
}

fn node_value(
    eve: &EveData,
    skill: LevelPair,
    graph: &BTreeMap<LevelPair, DepEntry>,
    priority: &BTreeMap<TypeId, f64>,
    memo: &mut BTreeMap<LevelPair, f64>,
) -> Result<f64, SkillPlanError> {
    if let Some(&value) = memo.get(&skill) {
        return Ok(value);
    }

    let ty = eve.require(skill.0)?;
    let multiplier = ty
        .attribute(AttributeId::TRAINING_TIME_MULTIPLIER)
        .ok_or(SkillPlanError::MissingTrainingTime(skill.0))?;
    // The legacy waitlist read attributes as 32-bit floats; mirror its rounding.
    let multiplier = f64::from(multiplier as f32);
    let sp_needed = 250. * multiplier * f64::sqrt(32.).powi(i32::from(skill.1) - 1);
    let mut value = priority.get(&skill.0).copied().unwrap_or(1.) / sp_needed;

    if let Some(entry) = graph.get(&skill) {
        for &dependee in &entry.dependees {
            value += node_value(eve, dependee, graph, priority, memo)? / 2.;
        }
    }

    memo.insert(skill, value);
    Ok(value)
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Ready {
    value: i64,
    level: SkillLevel,
    skill: TypeId,
}

/// Topological order of the graph, always taking the most valuable ready node next.
fn flatten(mut graph: BTreeMap<LevelPair, DepEntry>) -> Vec<LevelPair> {
    let mut ready: BinaryHeap<Ready> = graph
        .iter()
        .filter(|(_, entry)| entry.unsatisfied_requirements == 0)
        .map(|(&(skill, level), entry)| Ready {
            value: entry.value,
            level,
            skill,
        })
        .collect();

    let mut order = Vec::with_capacity(graph.len());
    while let Some(next) = ready.pop() {
        let pair = (next.skill, next.level);
        order.push(pair);
        let Some(entry) = graph.remove(&pair) else {
            continue;
        };
        for dependee in entry.dependees {
            if let Some(dependee_entry) = graph.get_mut(&dependee) {
                dependee_entry.unsatisfied_requirements -= 1;
                if dependee_entry.unsatisfied_requirements == 0 {
                    ready.push(Ready {
                        value: dependee_entry.value,
                        level: dependee.1,
                        skill: dependee.0,
                    });
                }
            }
        }
    }
    order
}
