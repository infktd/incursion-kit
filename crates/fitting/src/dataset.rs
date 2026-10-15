//! A consistent snapshot of every data file, resolved against the static data.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use wl_eve_data::{EveData, ModuleInfo, TypeId};

use crate::{
    Categories, CommunityRules, DataError, DataFile, DataSources, DoctrineFit, DoctrineFits,
    Fitting, FittingNote, FittingsResponse, ImplantSet, ModuleRules, SkillData, SkillPlan,
    SkillPlanError, SkillPlans,
    diff::{DiffResult, diff},
    fittings, matcher,
    skillplans::{LevelPair, PlanContext, PlanProblem, parse_skill_plans},
};

/// Every data file, parsed and resolved. Immutable; a reload builds a new one.
#[derive(Debug)]
pub struct DataSet {
    eve: Arc<EveData>,
    pub categories: Categories,
    pub fits: DoctrineFits,
    pub public_tags: BTreeSet<String>,
    pub modules: ModuleRules,
    pub skills: SkillData,
    pub skill_plans: SkillPlans,
    pub fit_notes: Vec<FittingNote>,
    pub implants: Vec<ImplantSet>,
    pub rules: CommunityRules,
    /// `GET /api/fittings`.
    pub fittings: FittingsResponse,
    /// `GET /api/module/preload`: every type used by a doctrine fit.
    pub module_preload: BTreeMap<TypeId, ModuleInfo>,
    /// Problems that did not stop loading (for example skill plans that fail to build).
    pub warnings: Vec<String>,
}

impl DataSet {
    /// Parses every data file and resolves it against the static data. Fails on the first
    /// malformed file, or with every unknown type name across all files.
    pub fn load(eve: Arc<EveData>, sources: &DataSources) -> Result<Self, DataError> {
        let mut names = eve.resolver();

        let categories = Categories::load(sources.get(DataFile::Categories)?, &mut names)?;
        let fits = DoctrineFits::load(sources.get(DataFile::Fits)?, &eve)?;
        let public_tags = fittings::load_public_tags(sources.get(DataFile::Tags)?)?;
        let modules = ModuleRules::load(sources.get(DataFile::Modules)?, &mut names)?;
        let skills = SkillData::load(sources.get(DataFile::Skills)?, &fits, &mut names)?;
        let fit_notes = fittings::load_notes(sources.get(DataFile::FitNotes)?)?;
        let implants = fittings::load_implants(sources.get(DataFile::Implants)?)?;
        let rules = CommunityRules::load(sources.get(DataFile::Rules)?, &mut names)?;
        let plans = parse_skill_plans(sources.get(DataFile::SkillPlans)?)?;
        names.finish()?;

        let mut warnings = Vec::new();
        warnings.extend(check_skill_sets(&rules, &skills));
        warnings.extend(check_categories(&rules, &categories));
        warnings.extend(fittings::check_implant_links(&implants, &fits));

        let context = PlanContext {
            eve: &eve,
            fits: &fits,
            skills: &skills,
            rules: &rules.skill_plans,
        };
        let (skill_plans, plan_warnings) = context.build_all(plans);
        warnings.extend(plan_warnings);
        for warning in &warnings {
            tracing::warn!("{warning}");
        }

        let fittings = fittings::fittings_response(
            &eve,
            &fits,
            &categories,
            &fit_notes,
            &implants,
            &rules.fittings,
        )
        .map_err(|error| DataError::invalid(DataFile::Fits.file_name(), error.to_string()))?;
        let module_preload = eve.module_info(fits.used_type_ids());

        Ok(Self {
            categories,
            fits,
            public_tags,
            modules,
            skills,
            skill_plans,
            fit_notes,
            implants,
            rules,
            fittings,
            module_preload,
            warnings,
            eve,
        })
    }

    /// The static data this data set was resolved against.
    pub fn eve(&self) -> &EveData {
        &self.eve
    }

    /// The static data, shared.
    pub fn eve_arc(&self) -> &Arc<EveData> {
        &self.eve
    }

    /// The doctrine fit closest to `fit` (same hull), with the differences.
    pub fn find_fit(&self, fit: &Fitting) -> Option<(&DoctrineFit, DiffResult)> {
        matcher::find_fit(fit, &self.fits, &self.modules)
    }

    /// How `actual` differs from the doctrine fit `expect`.
    pub fn diff(&self, expect: &Fitting, actual: &Fitting) -> DiffResult {
        diff(expect, actual, &self.modules)
    }

    /// The name of the implant set a doctrine fit uses (`fits` of `implants.yaml`), if any.
    pub fn implant_set_of(&self, fit_name: &str) -> Option<&str> {
        self.implants
            .iter()
            .find(|set| set.fits.iter().any(|fit| fit == fit_name))
            .map(|set| set.name.as_str())
    }

    /// The category id of a fit, falling back to the default category.
    pub fn categorize(&self, fit: &Fitting) -> &str {
        self.categories
            .categorize(fit)
            .unwrap_or(&self.rules.fit_check.default_category)
    }

    fn plan_context(&self) -> PlanContext<'_> {
        PlanContext {
            eve: &self.eve,
            fits: &self.fits,
            skills: &self.skills,
            rules: &self.rules.skill_plans,
        }
    }

    /// Ordered training steps of a plan.
    pub fn build_plan(&self, plan: &SkillPlan) -> Result<Vec<LevelPair>, SkillPlanError> {
        self.plan_context().build_plan(plan)
    }

    /// Validates a plan before an admin saves it.
    pub fn validate_plan(&self, plan: &SkillPlan) -> Result<(), SkillPlanError> {
        self.plan_context().validate_plan(plan)
    }

    /// Every validation problem of a plan, with field paths.
    pub fn plan_problems(&self, plan: &SkillPlan) -> Vec<PlanProblem> {
        self.plan_context().plan_problems(plan)
    }

    /// The hulls (or hull variants) a plan is for, as `GET /api/skills/plans` lists them.
    pub fn plan_ships(&self, plan: &SkillPlan) -> Vec<wl_core::types::Hull> {
        self.plan_context().ships(plan)
    }
}

/// Skill sets named by `rules.yaml` should exist in `skills.yaml`; plans using a missing one are
/// skipped.
fn check_skill_sets(rules: &CommunityRules, skills: &SkillData) -> Option<String> {
    let plan_rules = &rules.skill_plans;
    let mut referenced = vec![("generic_skill_set", plan_rules.generic_skill_set.as_str())];
    for variants in &plan_rules.hull_variants {
        referenced.push(("hull_variants", variants.default.as_str()));
        for variant in &variants.by_fit_name {
            referenced.push(("hull_variants", variant.skill_set.as_str()));
        }
    }
    let unknown: Vec<String> = referenced
        .into_iter()
        .filter(|(_, name)| !skills.requirements.contains_key(*name))
        .map(|(field, name)| format!("skill_plans.{field}: {name:?}"))
        .collect();
    (!unknown.is_empty()).then(|| {
        format!(
            "rules.yaml: skill sets not defined in skills.yaml requirements: {}",
            unknown.join(", ")
        )
    })
}

/// Category ids named by `rules.yaml` should exist in `categories.yaml`.
fn check_categories(rules: &CommunityRules, categories: &Categories) -> Option<String> {
    let mut referenced = vec![rules.fit_check.default_category.as_str()];
    referenced.extend(rules.war.blocked_categories.iter().map(String::as_str));
    let unknown: Vec<&str> = referenced
        .into_iter()
        .filter(|id| categories.name_of(id).is_none())
        .collect();
    (!unknown.is_empty()).then(|| {
        format!(
            "rules.yaml: categories not defined in categories.yaml: {}",
            unknown.join(", ")
        )
    })
}
