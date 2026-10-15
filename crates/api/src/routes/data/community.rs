//! Categories and their rules (`categories.yaml`), public tags (`tags.yaml`) and community
//! rules (`rules.yaml`), each edited as one document. Administrators only.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_fitting::DataFile;
use wl_fitting::editor::{self, CategoriesDocument, RulesDocument, TagsDocument};

use super::{ADMIN, WriteQuery, edit, read, unreadable};
use crate::extract::Query;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount, JsonBody, ValidationReport};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_categories, put_categories))
        .routes(routes!(get_tags, put_tags))
        .routes(routes!(get_rules, put_rules))
}

/// Get the categories
///
/// `categories.yaml`: the waitlist categories in display order and the rules (in order; the
/// first matching rule wins) that put a fit in a category by its hull or a fitted module.
/// Requires `commanders-manage:admin`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/categories",
    operation_id = "data_get_categories",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The categories and rules.", body = CategoriesDocument)),
)]
async fn get_categories(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<CategoriesDocument>> {
    account.require_access(ADMIN)?;
    let (content, _) = read(&state, DataFile::Categories).await?;
    Ok(Json(editor::read_categories(&content).map_err(unreadable)?))
}

/// Replace the categories
///
/// Replaces `categories.yaml`. Category ids are unique without whitespace; rule items must be
/// type names and rule categories must be listed. Categories named by `rules.yaml` but missing
/// here are warnings. The file is re-serialized (its leading comment block is kept, other
/// comments are lost), backed up and written atomically; `data_update` is published.
/// `?dry_run=true` only checks. Requires `commanders-manage:admin`. An extension beyond the
/// legacy API.
#[utoipa::path(
    put,
    path = "/api/data/categories",
    operation_id = "data_put_categories",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    params(WriteQuery),
    request_body = CategoriesDocument,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths (`rules[3].item`).", body = ValidationReport),
    ),
)]
async fn put_categories(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(document): JsonBody<CategoriesDocument>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(ADMIN)?;
    edit(
        &state,
        DataFile::Categories,
        query.dry_run,
        move |content, data| editor::write_categories(content, data.eve(), &document),
    )
    .await
}

/// Get the public tags
///
/// `tags.yaml`: the waitlist tags every pilot may see. Requires `commanders-manage:admin`. An
/// extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/tags",
    operation_id = "data_get_tags",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The public tags.", body = TagsDocument)),
)]
async fn get_tags(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<TagsDocument>> {
    account.require_access(ADMIN)?;
    let (content, _) = read(&state, DataFile::Tags).await?;
    Ok(Json(editor::read_tags(&content).map_err(unreadable)?))
}

/// Replace the public tags
///
/// Replaces `tags.yaml`. Tags are unique and contain no commas. Saved like the categories.
/// Requires `commanders-manage:admin`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/data/tags",
    operation_id = "data_put_tags",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    params(WriteQuery),
    request_body = TagsDocument,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths (`public_tags[2]`).", body = ValidationReport),
    ),
)]
async fn put_tags(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(document): JsonBody<TagsDocument>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(ADMIN)?;
    edit(&state, DataFile::Tags, query.dry_run, move |content, _| {
        editor::write_tags(content, &document)
    })
    .await
}

#[derive(Serialize, ToSchema)]
struct RulesResponse {
    /// Whether the data directory has its own `rules.yaml` (otherwise the built-in rules
    /// apply, and saving creates the file).
    custom: bool,
    /// The rules in force. A file without the optional `fleet` section shows the built-in one.
    rules: RulesDocument,
}

/// Get the community rules
///
/// `rules.yaml` as typed sections (`fit_check`, `fittings`, `war`, `skill_plans`, `fleet`), or
/// the built-in rules when the data directory has none. Requires `commanders-manage:admin`. An
/// extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/rules",
    operation_id = "data_get_rules",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    responses((status = 200, description = "The rules.", body = RulesResponse)),
)]
async fn get_rules(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<RulesResponse>> {
    account.require_access(ADMIN)?;
    let custom = state.data_store()?.path(DataFile::Rules).exists();
    let (content, _) = read(&state, DataFile::Rules).await?;
    Ok(Json(RulesResponse {
        custom,
        rules: editor::read_rules(&content).map_err(unreadable)?,
    }))
}

/// Replace the community rules
///
/// Writes `rules.yaml` (creating it when the built-in rules applied). Every type name must
/// exist, `implant_module_rules` must name sets of `implant_sets`, `default_tank_level` must be
/// one of `tank_levels`, MOTD role names are unique without whitespace or `:`. Skill sets and
/// categories that do not exist are warnings. The file is re-serialized: its leading comment
/// block is kept, the per-setting comments and YAML anchors of the built-in file are not.
/// Backed up, written atomically; `data_update` is published. `?dry_run=true` only checks.
/// Requires `commanders-manage:admin`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/data/rules",
    operation_id = "data_put_rules",
    tag = tags::DATA,
    security(("session" = ["commanders-manage:admin"])),
    params(WriteQuery),
    request_body = RulesDocument,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths (`fit_check.badge_auto_approve[0].hull`).", body = ValidationReport),
    ),
)]
async fn put_rules(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(document): JsonBody<RulesDocument>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(ADMIN)?;
    edit(
        &state,
        DataFile::Rules,
        query.dry_run,
        move |content, data| editor::write_rules(content, data.eve(), &document),
    )
    .await
}
