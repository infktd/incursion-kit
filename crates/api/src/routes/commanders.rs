//! Commander (role) management (DB table `admin`, one role per character).
//!
//! - `GET /api/commanders` (`commanders-view`): every role assignment plus counts per role.
//! - `POST /api/commanders` `{character_id, role}` (`commanders-manage`, and
//!   `commanders-manage:<role>` or 403): grant a role; text `Ok`. 404 (empty body) for an unknown
//!   character, 400 if it already has a role.
//! - `GET /api/commanders/roles` (`commanders-manage`): the roles the caller may grant.
//! - `GET /api/commanders/{character_id}` (`commanders-manage`): the role as plain text, or 404
//!   with an empty body.
//! - `DELETE /api/commanders/{character_id}` (`commanders-manage`, and
//!   `commanders-manage:<their role>` or 403): revoke; not your own (400). Text `Ok`, also when the
//!   character had no role.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::permissions::{ROLES, access_keys};
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, assign))
        .routes(routes!(assignable))
        .routes(routes!(lookup, revoke))
}

#[derive(Deserialize, ToSchema)]
#[schema(as = AssignCommanderRequest)]
struct RequestPayload {
    /// The character to grant the role to (required; a missing id is a 400, not a 422).
    character_id: Option<i64>,
    /// One of `user`, `trainee`, `trainee-advanced`, `fc`, `fc+`, `admin`.
    role: String,
}

#[derive(Serialize, ToSchema)]
struct CmdCharacter {
    id: i64,
    name: String,
}

/// A role assignment.
#[derive(Serialize, ToSchema)]
struct Commander {
    character: CmdCharacter,
    role: String,
    /// Who granted the role.
    granted_by: CmdCharacter,
    /// When the role was granted (Unix seconds).
    granted_at: i64,
}

/// A role and how many characters hold it.
#[derive(Serialize, sqlx::FromRow, ToSchema)]
struct CommanderRank {
    /// The role name.
    name: String,
    member_count: i64,
}

#[derive(Serialize, ToSchema)]
struct CommanderList {
    /// Every role assignment, by character id.
    commanders: Vec<Commander>,
    /// The roles in use with their member counts, by role name.
    filters: Vec<CommanderRank>,
}

#[derive(sqlx::FromRow)]
struct CommanderRow {
    role: String,
    granted_at: i64,
    id: i64,
    name: String,
    admin_id: i64,
    admin_name: String,
}

async fn role_of(state: &AppState, character_id: i64) -> ApiResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT role FROM admin WHERE character_id = ?")
            .bind(character_id)
            .fetch_optional(state.db())
            .await?,
    )
}

/// List commanders
///
/// Every role assignment (one role per character), with who granted it, plus the number of
/// characters per role. Assignments whose character or granter is missing from the character
/// table are not listed. Requires `commanders-view`.
#[utoipa::path(
    get,
    path = "/api/commanders",
    operation_id = "list_commanders",
    tag = tags::COMMUNITY,
    security(("session" = ["commanders-view"])),
    responses((status = 200, description = "The role assignments.", body = CommanderList)),
)]
async fn list(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<CommanderList>> {
    account.require_access("commanders-view")?;
    let filters: Vec<CommanderRank> = sqlx::query_as(
        "SELECT role AS name, COUNT(role) AS member_count FROM admin GROUP BY role ORDER BY role",
    )
    .fetch_all(state.db())
    .await?;
    let rows: Vec<CommanderRow> = sqlx::query_as(
        "SELECT admin.role, admin.granted_at, fc.id, fc.name, a.id AS admin_id, a.name AS admin_name
         FROM admin
         JOIN `character` fc ON admin.character_id = fc.id
         JOIN `character` a ON admin.granted_by_id = a.id
         ORDER BY admin.character_id",
    )
    .fetch_all(state.db())
    .await?;
    let commanders = rows
        .into_iter()
        .map(|row| Commander {
            character: CmdCharacter {
                id: row.id,
                name: row.name,
            },
            role: row.role,
            granted_by: CmdCharacter {
                id: row.admin_id,
                name: row.admin_name,
            },
            granted_at: row.granted_at,
        })
        .collect();
    Ok(Json(CommanderList {
        commanders,
        filters,
    }))
}

/// Grant a role
///
/// Gives a character a role. Requires `commanders-manage`, plus `commanders-manage:<role>` for
/// the role being granted (403 otherwise). A character can only have one role: revoke the
/// current one first.
#[utoipa::path(
    post,
    path = "/api/commanders",
    operation_id = "assign_commander",
    tag = tags::COMMUNITY,
    security(("session" = ["commanders-manage"])),
    request_body = RequestPayload,
    responses(
        (
            status = 200,
            description = "Role granted.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Request body missing required property character_id`, `The FC rank \"<role>\" does not exist`, or `Cannot assign \"<role>\" to <name> as they already have a role`.",
            body = String,
            content_type = "text/plain",
        ),
        (
            status = 403,
            description = "`You do not have permission to grant the role \"<role>\"`.",
            body = String,
            content_type = "text/plain",
        ),
        (status = 404, description = "Unknown character (empty body)."),
    ),
)]
async fn assign(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(body): JsonBody<RequestPayload>,
) -> ApiResult<&'static str> {
    account.require_access("commanders-manage")?;
    let Some(character_id) = body.character_id else {
        return Err(ApiError::bad_request(
            "Request body missing required property character_id",
        ));
    };
    if access_keys(&body.role).is_none() {
        return Err(ApiError::bad_request(format!(
            "The FC rank \"{}\" does not exist",
            body.role
        )));
    }
    if !account.has_access(&format!("commanders-manage:{}", body.role)) {
        return Err(ApiError::forbidden(format!(
            "You do not have permission to grant the role \"{}\"",
            body.role
        )));
    }

    let name: Option<String> = sqlx::query_scalar("SELECT name FROM `character` WHERE id = ?")
        .bind(character_id)
        .fetch_optional(state.db())
        .await?;
    let Some(name) = name else {
        return Err(ApiError::not_found(""));
    };
    if role_of(&state, character_id).await?.is_some() {
        return Err(ApiError::bad_request(format!(
            "Cannot assign \"{}\" to {name} as they already have a role",
            body.role
        )));
    }

    sqlx::query(
        "INSERT INTO admin (character_id, role, granted_at, granted_by_id) VALUES (?, ?, ?, ?)",
    )
    .bind(character_id)
    .bind(&body.role)
    .bind(chrono::Utc::now().timestamp())
    .bind(account.id)
    .execute(state.db())
    .await?;
    Ok("Ok")
}

/// Grantable roles
///
/// The roles the caller may grant (those it holds `commanders-manage:<role>` for), lowest first.
/// The legacy order was effectively arbitrary for `fc+` and `admin`; the role order is used
/// here. Requires `commanders-manage`.
#[utoipa::path(
    get,
    path = "/api/commanders/roles",
    operation_id = "assignable_roles",
    tag = tags::COMMUNITY,
    security(("session" = ["commanders-manage"])),
    responses((
        status = 200,
        description = "Role names.",
        body = Vec<String>,
        example = json!(["trainee", "trainee-advanced", "fc"]),
    )),
)]
async fn assignable(account: AuthenticatedAccount) -> ApiResult<Json<Vec<&'static str>>> {
    account.require_access("commanders-manage")?;
    let roles = ROLES
        .iter()
        .copied()
        .filter(|role| account.has_access(&format!("commanders-manage:{role}")))
        .collect();
    Ok(Json(roles))
}

/// Role of a character
///
/// The character's role as plain text. Requires `commanders-manage`.
#[utoipa::path(
    get,
    path = "/api/commanders/{character_id}",
    operation_id = "get_commander_role",
    tag = tags::COMMUNITY,
    security(("session" = ["commanders-manage"])),
    params(("character_id" = i64, Path, description = "Character id.")),
    responses(
        (
            status = 200,
            description = "The role name.",
            body = String,
            content_type = "text/plain",
            example = "fc",
        ),
        (
            status = 404,
            description = "The character has no role (empty body), or the id is malformed.",
        ),
    ),
)]
async fn lookup(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(character_id): Path<i64>,
) -> ApiResult<String> {
    account.require_access("commanders-manage")?;
    role_of(&state, character_id)
        .await?
        .ok_or_else(|| ApiError::not_found(""))
}

/// Revoke a role
///
/// Removes the character's role. Requires `commanders-manage`, plus
/// `commanders-manage:<their role>` (403 otherwise). You cannot revoke your own role. Succeeds
/// (`Ok`) also when the character has no role.
#[utoipa::path(
    delete,
    path = "/api/commanders/{character_id}",
    operation_id = "revoke_commander",
    tag = tags::COMMUNITY,
    security(("session" = ["commanders-manage"])),
    params(("character_id" = i64, Path, description = "Character id.")),
    responses(
        (
            status = 200,
            description = "Role revoked (or there was none).",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`You cannot revoke your own rank.`",
            body = String,
            content_type = "text/plain",
        ),
        (
            status = 403,
            description = "`You do not have permission to revoke the role \"<role>\"`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn revoke(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(character_id): Path<i64>,
) -> ApiResult<&'static str> {
    account.require_access("commanders-manage")?;
    if account.id == character_id {
        return Err(ApiError::bad_request("You cannot revoke your own rank."));
    }
    if let Some(role) = role_of(&state, character_id).await? {
        if !account.has_access(&format!("commanders-manage:{role}")) {
            return Err(ApiError::forbidden(format!(
                "You do not have permission to revoke the role \"{role}\""
            )));
        }
        sqlx::query("DELETE FROM admin WHERE character_id = ?")
            .bind(character_id)
            .execute(state.db())
            .await?;
    }
    Ok("Ok")
}
