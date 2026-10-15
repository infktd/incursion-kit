//! What the editors' pickers need: type search, the names the data files refer to, and a dry
//! run of skill plans.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Hull, WaitlistCategory};
use wl_eve_data::{SkillLevel, TypeId};
use wl_fitting::editor::{self, FieldError, TypeKind, TypeSearchHit};
use wl_fitting::{DataFile, SkillPlan, is_skill_group};

use super::{ADMIN, FITS_MANAGE, read};
use crate::extract::Query;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody, ValidationReport};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(search_types))
        .routes(routes!(options))
        .routes(routes!(validate_plan))
}

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 100;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct TypeQuery {
    /// Part of the name, case-insensitive (empty: every type of the kinds).
    #[serde(default)]
    q: String,
    /// Comma-separated kinds: `ship`, `module` (including rigs), `subsystem`, `charge`, `drone`,
    /// `implant` (and boosters), `skill`, `other`. Any kind when absent.
    kind: Option<String>,
    /// At most this many (default 20, at most 100).
    limit: Option<usize>,
}

#[derive(Serialize, ToSchema)]
struct TypeSearchResponse {
    /// Exact matches first, then names starting with the query, then names with a word
    /// starting with it, then other names containing it; alphabetical within each.
    types: Vec<TypeSearchHit>,
}

/// Search types
///
/// Published types of the static data by name, for pickers: hulls, modules, implants, skills.
/// `name` is the exact name the data files use. Requires `fits-manage`. An extension beyond the
/// legacy API.
#[utoipa::path(
    get,
    path = "/api/data/types",
    operation_id = "data_search_types",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(TypeQuery),
    responses(
        (status = 200, description = "The matching types.", body = TypeSearchResponse),
        (status = 400, description = "An unknown kind (path `kind`).", body = ValidationReport),
    ),
)]
async fn search_types(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<TypeQuery>,
) -> ApiResult<Json<TypeSearchResponse>> {
    account.require_access(FITS_MANAGE)?;
    let mut kinds = Vec::new();
    for kind in query.kind.iter().flat_map(|kinds| kinds.split(',')) {
        let kind = kind.trim();
        if kind.is_empty() {
            continue;
        }
        let parsed: TypeKind = serde_json::from_value(serde_json::Value::String(kind.to_string()))
            .map_err(|_| {
                ApiError::invalid(
                    false,
                    vec![FieldError {
                        path: "kind".into(),
                        message: format!("unknown kind {kind:?}"),
                        line: None,
                    }],
                    Vec::new(),
                )
            })?;
        kinds.push(parsed);
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let data = state.data_store()?.get();
    Ok(Json(TypeSearchResponse {
        types: editor::search_types(data.eve(), &query.q, &kinds, limit),
    }))
}

/// Kind of a skill set of `skills.yaml`.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum SkillSetKind {
    /// A hull (`Vindicator`).
    Hull,
    /// A tank variant of a hull (`Armor Kronos`, see `skill_plans.variant_prefixes`).
    Variant,
    /// A shared group merged into hulls (`_armor`).
    Group,
}

#[derive(Serialize, ToSchema)]
struct SkillSetOption {
    name: String,
    kind: SkillSetKind,
}

#[derive(Serialize, ToSchema)]
struct HullOption {
    #[schema(value_type = i32)]
    id: TypeId,
    name: String,
    /// Its doctrine fits, in file order.
    fits: Vec<String>,
}

#[derive(Serialize, ToSchema)]
struct EditorOptions {
    /// Hulls with doctrine fits, by name.
    hulls: Vec<HullOption>,
    /// The skill sets of `skills.yaml` (keys of `requirements`), by name.
    skill_sets: Vec<SkillSetOption>,
    /// Tiers of `type: skills` plan steps.
    skill_tiers: Vec<String>,
    /// Levels of `type: tank` plan steps, in order.
    tank_levels: Vec<String>,
    /// Waitlist categories, in display order.
    categories: Vec<WaitlistCategory>,
    /// Public tags, sorted.
    public_tags: Vec<String>,
    /// Sections of `fits.dat`, in file order.
    fit_sections: Vec<String>,
    /// Fits whose name contains this are hidden from the public list.
    hidden_fit_marker: String,
    /// Skill plan names, in file order.
    skill_plans: Vec<String>,
    /// Implant set names, in file order.
    implant_sets: Vec<String>,
}

/// Editor options
///
/// The names the data files refer to, for the editors' pickers: hulls with their doctrine fits,
/// skill sets (hulls, tank variants and shared groups), skill tiers, tank levels, categories,
/// public tags, `fits.dat` sections, skill plans and implant sets. Types are searched with
/// `GET /api/data/types`. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/options",
    operation_id = "data_options",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    responses((status = 200, description = "The options.", body = EditorOptions)),
)]
async fn options(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<EditorOptions>> {
    account.require_access(FITS_MANAGE)?;
    let (fits_dat, data) = read(&state, DataFile::Fits).await?;
    let eve = data.eve();
    let mut hulls: Vec<HullOption> = data
        .fits
        .hulls()
        .map(|hull| HullOption {
            id: hull,
            name: eve.name_of(hull).unwrap_or_default().to_string(),
            fits: data
                .fits
                .for_hull(hull)
                .iter()
                .map(|fit| fit.name.clone())
                .collect(),
        })
        .collect();
    hulls.sort_by(|a, b| a.name.cmp(&b.name));
    let plan_rules = &data.rules.skill_plans;
    let skill_sets = data
        .skills
        .requirements
        .keys()
        .map(|name| SkillSetOption {
            kind: if is_skill_group(name) {
                SkillSetKind::Group
            } else if plan_rules.is_variant_name(name) {
                SkillSetKind::Variant
            } else {
                SkillSetKind::Hull
            },
            name: name.clone(),
        })
        .collect();
    let mut fit_sections: Vec<String> = Vec::new();
    for fit in editor::doctrine_fits(&fits_dat, &data) {
        if let Some(section) = fit.section
            && !fit_sections.contains(&section)
        {
            fit_sections.push(section);
        }
    }
    Ok(Json(EditorOptions {
        hulls,
        skill_sets,
        skill_tiers: ["min", "elite", "gold"].map(String::from).to_vec(),
        tank_levels: plan_rules
            .tank_levels
            .iter()
            .map(|level| level.name.clone())
            .collect(),
        categories: data.categories.categories.clone(),
        public_tags: data.public_tags.iter().cloned().collect(),
        fit_sections,
        hidden_fit_marker: data.rules.fittings.hidden_fit_marker.clone(),
        skill_plans: data
            .skill_plans
            .plans
            .iter()
            .map(|plan| plan.name.clone())
            .collect(),
        implant_sets: data.implants.iter().map(|set| set.name.clone()).collect(),
    }))
}

#[derive(Deserialize, ToSchema)]
struct ValidatePlanRequest {
    plan: SkillPlan,
    /// The plan's current name when it is being updated (renaming onto another plan is an
    /// error); absent for a new plan (whose name must be free).
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct PlanValidation {
    /// Always `true` (an invalid plan is a `400` [`ValidationReport`]).
    valid: bool,
    /// The training steps the plan builds to (`[skill id, level]`, in order), or `null` when it
    /// does not build (see `warnings`).
    #[schema(value_type = Option<Vec<(i32, i8)>>)]
    levels: Option<Vec<(TypeId, SkillLevel)>>,
    /// The hulls (or hull variants) the plan is for.
    ships: Vec<Hull>,
    /// E.g. a plan that does not build: it can be saved, but `GET /api/skills/plans` leaves it
    /// out.
    warnings: Vec<String>,
}

/// Validate a skill plan
///
/// A dry run of `POST /api/admin/skillplans` (no `name`) or `PUT /api/admin/skillplans/{name}`:
/// every problem with its field path (`name`, `plan[2].fit`; messages as the skill plan
/// endpoints word them), and for a valid plan its training steps and hulls. Nothing is
/// written. Requires `commanders-manage:admin`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/data/skill-plans/validate",
    operation_id = "data_validate_skill_plan",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    request_body = ValidatePlanRequest,
    responses(
        (status = 200, description = "Valid.", body = PlanValidation),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
    ),
)]
async fn validate_plan(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<ValidatePlanRequest>,
) -> ApiResult<Json<PlanValidation>> {
    account.require_access(ADMIN)?;
    let data = state.data_store()?.get();
    let plan = &input.plan;
    let mut errors: Vec<FieldError> = data
        .plan_problems(plan)
        .into_iter()
        .map(|problem| FieldError {
            path: problem.path(),
            message: problem.message,
            line: None,
        })
        .collect();
    let others: BTreeSet<&str> = data
        .skill_plans
        .plans
        .iter()
        .map(|other| other.name.as_str())
        .filter(|other| Some(*other) != input.name.as_deref())
        .collect();
    if let Some(current) = &input.name
        && !data
            .skill_plans
            .plans
            .iter()
            .any(|other| &other.name == current)
    {
        return Err(ApiError::not_found("Skill plan not found"));
    }
    if others.contains(plan.name.as_str()) {
        errors.push(FieldError {
            path: "name".into(),
            message: format!("Skill plan with name '{}' already exists", plan.name),
            line: None,
        });
    }
    if !errors.is_empty() {
        return Err(ApiError::invalid(true, errors, Vec::new()));
    }
    let (levels, warnings) = match data.build_plan(plan) {
        Ok(levels) => (Some(levels), Vec::new()),
        Err(error) => (
            None,
            vec![format!(
                "the plan does not build ({error}); it would be left out of the skill plans"
            )],
        ),
    };
    Ok(Json(PlanValidation {
        valid: true,
        levels,
        ships: data.plan_ships(plan),
        warnings,
    }))
}
