//! Skill plans (`skillplan.yaml` in the data directory).
//!
//! - `GET /api/skills/plans` (any logged-in account): `{plans: [{source, levels, ships}]}`; plans
//!   that fail to build are left out.
//!
//! Admin endpoints (`commanders-manage:admin`). Every write validates the plan (400 `skill plan
//! error: validation error: …`), backs up the file, writes it atomically and swaps in the new
//! data set:
//! - `GET /api/admin/skillplans`: every plan as written.
//! - `GET /api/admin/skillplans/{name}`: one plan; 404 `Skill plan not found`.
//! - `POST /api/admin/skillplans` `{plan}`: text `Skill plan created successfully`; 400 if the
//!   name is taken.
//! - `PUT /api/admin/skillplans/{name}` `{plan}` (may rename): text `Skill plan updated
//!   successfully`; 404 or 400.
//! - `DELETE /api/admin/skillplans/{name}`: text `Skill plan deleted successfully`; 404.
//! - `GET /api/admin/skillplans/raw`: the YAML file as text.
//! - `POST /api/admin/skillplans/raw` (raw YAML body): text `Skill plans saved successfully`;
//!   400 for YAML that doesn't parse or doesn't load.
//!
//! Unlike the legacy waitlist (which re-read the file on every request), reads come from the
//! loaded data set, which every write here and the admin data-file editor keep current.

use std::sync::Arc;

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_fitting::{
    DataError, DataFile, DataStore, SkillPlan, SkillPlanError, SkillPlansResponse,
    parse_skill_plans, skill_plans_to_yaml,
};

use super::blocking;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(plans))
        .routes(routes!(admin_list, admin_create))
        .routes(routes!(admin_raw_get, admin_raw_save))
        .routes(routes!(admin_get, admin_update, admin_delete))
}

const ADMIN: &str = "commanders-manage:admin";
const NOT_FOUND: &str = "Skill plan not found";

fn plan_error(err: SkillPlanError) -> ApiError {
    ApiError::SkillPlan(Box::new(err))
}

/// Saving failed: `skill plan error: file write error: …`, as in the legacy waitlist.
fn write_error(err: DataError) -> ApiError {
    plan_error(SkillPlanError::FileWrite(err.to_string()))
}

/// Skill plans
///
/// Every skill plan of `skillplan.yaml` with its training steps in order (deduplicated
/// `[skill id, level]` pairs) and the hulls it is for. Plans that fail to build against the
/// current data are left out. Requires login (any account).
#[utoipa::path(
    get,
    path = "/api/skills/plans",
    operation_id = "list_skill_plans",
    tag = tags::DOCTRINE,
    security(("session" = [])),
    responses((status = 200, description = "The skill plans.", body = SkillPlansResponse)),
)]
async fn plans(
    State(state): State<AppState>,
    _account: AuthenticatedAccount,
) -> ApiResult<Json<SkillPlansResponse>> {
    Ok(Json(state.data_store()?.get().skill_plans.response.clone()))
}

/// List skill plans (admin)
///
/// Every plan of `skillplan.yaml` as written, in file order, including plans that fail to
/// build. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/skillplans",
    operation_id = "admin_list_skill_plans",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The plans.", body = Vec<SkillPlan>)),
)]
async fn admin_list(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<Vec<SkillPlan>>> {
    account.require_access(ADMIN)?;
    Ok(Json(state.data_store()?.get().skill_plans.plans.clone()))
}

/// Get a skill plan (admin)
///
/// One plan as written in `skillplan.yaml`. A plan named `raw` cannot be addressed by name
/// (that path is the raw file). Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/skillplans/{name}",
    operation_id = "admin_get_skill_plan",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("name" = String, Path, description = "Plan name.")),
    responses(
        (status = 200, description = "The plan.", body = SkillPlan),
        (status = 404, description = "`Skill plan not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn admin_get(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
) -> ApiResult<Json<SkillPlan>> {
    account.require_access(ADMIN)?;
    state
        .data_store()?
        .get()
        .skill_plans
        .plans
        .iter()
        .find(|p| p.name == name)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found(NOT_FOUND))
}

#[derive(Deserialize, ToSchema)]
struct PlanRequest {
    plan: SkillPlan,
}

/// Validates `plan` against the current data, applies `edit` to the current plans and saves.
async fn edit_plans(
    state: &AppState,
    plan: Option<&SkillPlan>,
    edit: impl FnOnce(&mut Vec<SkillPlan>) -> ApiResult<()>,
) -> ApiResult<()> {
    let store = state.data_store()?.clone();
    let data = store.get();
    if let Some(plan) = plan {
        data.validate_plan(plan).map_err(plan_error)?;
    }
    let mut plans = data.skill_plans.plans.clone();
    edit(&mut plans)?;
    let yaml = skill_plans_to_yaml(&plans).map_err(write_error)?;
    save(store, yaml).await
}

async fn save(store: Arc<DataStore>, yaml: String) -> ApiResult<()> {
    blocking(move || {
        store
            .replace_file(DataFile::SkillPlans, &yaml)
            .map_err(write_error)?;
        Ok(())
    })
    .await
}

fn name_taken(name: &str) -> ApiError {
    ApiError::bad_request(format!("Skill plan with name '{name}' already exists"))
}

/// Create a skill plan (admin)
///
/// Validates the plan against the current data (hulls, fits, skill sets, tiers, skills and
/// tank levels must exist), appends it to `skillplan.yaml` (backing up the old file, writing
/// atomically) and swaps in the new data set. Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/skillplans",
    operation_id = "admin_create_skill_plan",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    request_body = PlanRequest,
    responses(
        (status = 200, description = "Created.", body = String, content_type = "text/plain", example = "Skill plan created successfully"),
        (
            status = 400,
            description = "`skill plan error: validation error: …` for an invalid plan (e.g. \
                           `Plan name cannot be empty`, `Plan step 2: Fit 'x' not found for hull \
                           'y'`); `Skill plan with name '<name>' already exists`; \
                           `skill plan error: file write error: …` when the new file does not \
                           load or cannot be written.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn admin_create(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<PlanRequest>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    let plan = input.plan;
    edit_plans(&state, Some(&plan), |plans| {
        if plans.iter().any(|p| p.name == plan.name) {
            return Err(name_taken(&plan.name));
        }
        plans.push(plan.clone());
        Ok(())
    })
    .await?;
    Ok("Skill plan created successfully")
}

/// Update a skill plan (admin)
///
/// Replaces the plan called `name` with the given plan, which may have a different name
/// (renaming it). The new plan is validated first, so an invalid plan is a 400 even when
/// `name` does not exist. Saved like a create. Requires `commanders-manage:admin`.
#[utoipa::path(
    put,
    path = "/api/admin/skillplans/{name}",
    operation_id = "admin_update_skill_plan",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("name" = String, Path, description = "Current plan name.")),
    request_body = PlanRequest,
    responses(
        (status = 200, description = "Updated.", body = String, content_type = "text/plain", example = "Skill plan updated successfully"),
        (
            status = 400,
            description = "`skill plan error: validation error: …` for an invalid plan; \
                           `Skill plan with name '<name>' already exists` when renaming onto \
                           another plan; `skill plan error: file write error: …` when the new \
                           file does not load or cannot be written.",
            body = String,
            content_type = "text/plain",
        ),
        (status = 404, description = "`Skill plan not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn admin_update(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    JsonBody(input): JsonBody<PlanRequest>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    let plan = input.plan;
    edit_plans(&state, Some(&plan), |plans| {
        let index = plans
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| ApiError::not_found(NOT_FOUND))?;
        if plan.name != name && plans.iter().any(|p| p.name == plan.name) {
            return Err(name_taken(&plan.name));
        }
        plans[index] = plan.clone();
        Ok(())
    })
    .await?;
    Ok("Skill plan updated successfully")
}

/// Delete a skill plan (admin)
///
/// Removes the plan from `skillplan.yaml` (backing up the old file) and swaps in the new data
/// set. Requires `commanders-manage:admin`.
#[utoipa::path(
    delete,
    path = "/api/admin/skillplans/{name}",
    operation_id = "admin_delete_skill_plan",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("name" = String, Path, description = "Plan name.")),
    responses(
        (status = 200, description = "Deleted.", body = String, content_type = "text/plain", example = "Skill plan deleted successfully"),
        (status = 400, description = "`skill plan error: file write error: …` when the new file does not load or cannot be written.", body = String, content_type = "text/plain"),
        (status = 404, description = "`Skill plan not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn admin_delete(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    edit_plans(&state, None, |plans| {
        let before = plans.len();
        plans.retain(|p| p.name != name);
        if plans.len() == before {
            return Err(ApiError::not_found(NOT_FOUND));
        }
        Ok(())
    })
    .await?;
    Ok("Skill plan deleted successfully")
}

/// Raw skill plan file (admin)
///
/// `skillplan.yaml` as text, read from disk. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/skillplans/raw",
    operation_id = "admin_get_skill_plans_raw",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    responses(
        (status = 200, description = "The YAML file.", body = String, content_type = "text/plain"),
        (status = 400, description = "`Failed to read skillplan.yaml: …`", body = String, content_type = "text/plain"),
    ),
)]
async fn admin_raw_get(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<impl IntoResponse> {
    account.require_access(ADMIN)?;
    let store = state.data_store()?.clone();
    let content = blocking(move || {
        store
            .read_file(DataFile::SkillPlans)
            .map_err(|e| ApiError::bad_request(format!("Failed to read skillplan.yaml: {e}")))
    })
    .await?;
    Ok((
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        content,
    ))
}

/// Replace the skill plan file (admin)
///
/// Replaces `skillplan.yaml` with the request body. The YAML must parse and the whole data set
/// must load with it; then the old file is backed up, the new one written atomically and the
/// new data set swapped in. Plans that fail to build are accepted (and left out of
/// `GET /api/skills/plans`). Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/skillplans/raw",
    operation_id = "admin_save_skill_plans_raw",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    request_body(content = String, content_type = "text/plain", description = "The new `skillplan.yaml` (any content type is accepted)."),
    responses(
        (status = 200, description = "Saved.", body = String, content_type = "text/plain", example = "Skill plans saved successfully"),
        (
            status = 400,
            description = "`skill plan error: file write error: Invalid YAML: …` when the body \
                           does not parse; `skill plan error: file write error: …` when the data \
                           set does not load with it or the file cannot be written.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn admin_raw_save(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    body: String,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    parse_skill_plans(&body)
        .map_err(|e| plan_error(SkillPlanError::FileWrite(format!("Invalid YAML: {e}"))))?;
    save(state.data_store()?.clone(), body).await?;
    Ok("Skill plans saved successfully")
}
