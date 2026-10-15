//! Fittings and doctrine logic: EFT/DNA parsing, doctrine fits, waitlist categories, tags,
//! skill requirements and plans, implant guides and the fit checker.
//!
//! Community data lives in the data directory (`categories.yaml`, `fits.dat`, `modules.yaml`,
//! `skills.yaml`, ...). [`DataStore`] loads it into an immutable [`DataSet`] resolved against the
//! static data ([`wl_eve_data::EveData`]); readers take cheap snapshots, and reloads or admin
//! edits swap in a new data set only once it has loaded cleanly.
//!
//! Community-specific rules that the legacy waitlist hardcoded (badge auto-approval, implant/rig
//! combinations, hours-in-fleet limits, tag rules, war restrictions, skill plan variants, fleet
//! MOTD roles) live in `rules.yaml` ([`CommunityRules`]), with a built-in default reproducing the
//! legacy behaviour.

mod categories;
mod check;
mod dataset;
mod diff;
mod doctrine;
pub mod editor;
mod error;
pub mod example_data;
mod files;
mod fit;
mod fittings;
mod matcher;
mod modules;
mod rules;
mod skillplans;
mod skills;
mod store;
mod yaml;

pub use categories::{Categories, CategoryRule};
pub use check::{FitAnalysis, FitCheckOutput, PilotData, PilotSkills, SrpModifiedModule};
pub use dataset::DataSet;
pub use diff::{DiffResult, Replacements};
pub use doctrine::{DoctrineFit, DoctrineFits};
pub use error::DataError;
pub use files::{DEFAULT_RULES, DataFile, DataSources};
pub use fit::{FitError, Fitting};
pub use fittings::{
    DnaFitting, FittingNote, FittingsResponse, ImplantEntry, ImplantSet, ImplantSlot,
};
pub use modules::{
    CountCompare, DrugOverride, FitVariation, ModuleRules, SrpModifiedPricing, SrpModifiedRule,
    Variation, VariationCount, Variator,
};
pub use rules::{
    AccessTag, BadgeApproval, CommunityRules, FitCheckRules, FitNameVariant, FittingsRules,
    FleetRules, HullVariants, ImplantModuleRule, MotdRole, SkillPlanRules, TankLevel,
    TimeInFleetLimit, WarRules,
};
pub use skillplans::{
    LevelPair, PlanProblem, SkillPlan, SkillPlanError, SkillPlanLevel, SkillPlans,
    SkillPlansResponse, SkillPlansResponsePlan, parse_skill_plans, skill_plans_to_yaml,
};
pub use skills::{
    SkillCategories, SkillData, SkillRequirements, SkillTier, SkillTiers, is_skill_group,
};
pub use store::{DataStore, EditError, Validation};
