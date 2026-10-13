//! Character / corporation / alliance bans (DB table `ban`). All endpoints need `bans-manage`.
//!
//! - `GET /api/v2/bans`: active bans (`revoked_at` is the expiry; `revoked_by` always null).
//! - `POST /api/v2/bans` (`Ban` body): ban `entity`. The entity's name is looked up on ESI (the
//!   client's name is ignored), so `Account` bans cannot be created here. `revoked_at` is the
//!   expiry *day*; 11 hours (downtime) are added. Text `Ok`.
//! - `GET /api/v2/bans/{character_id}`: a character's full ban history, oldest first.
//! - `PATCH /api/v2/bans/{ban_id}` (`Ban` body): edit `reason`, `public_reason` and `revoked_at`
//!   (+11h) of an active ban; the editor becomes the issuer, `issued_at` becomes now. Text `Ok`.
//! - `DELETE /api/v2/bans/{ban_id}`: revoke now. Text `Ok`.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::EsiError;
use wl_core::types::Ban;
use wl_esi::character::EntityKind;

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody, ban};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        // One path for both ids: GET takes a character id, PATCH/DELETE a ban id.
        .routes(routes!(character_history, update, revoke))
}

/// Expiry days are sent as midnight; bans end at downtime (11:00 UTC).
const DOWNTIME_OFFSET: i64 = 60 * 60 * 11;

fn expiry(day: Option<i64>) -> Option<i64> {
    day.map(|day| day + DOWNTIME_OFFSET)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// List active bans
///
/// Every active ban, on any entity, oldest first. `revoked_at` is the expiry (`null` for a
/// permanent ban) and `revoked_by` is always `null`. Requires `bans-manage`.
#[utoipa::path(
    get,
    path = "/api/v2/bans",
    operation_id = "list_bans",
    tag = tags::COMMUNITY,
    security(("session" = ["bans-manage"])),
    responses((status = 200, description = "The active bans.", body = Vec<Ban>)),
)]
async fn list(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<Vec<Ban>>> {
    account.require_access("bans-manage")?;
    Ok(Json(ban::all_active_bans(state.db()).await?))
}

/// Ban an entity
///
/// Bans `entity` (`{id, category}` with category `Character`, `Corporation` or `Alliance`;
/// `name` is ignored, the name is looked up on ESI). `reason` is the internal reason,
/// `public_reason` the one shown to the pilot. `revoked_at` is the expiry *day* (midnight UTC,
/// Unix seconds; `null` for a permanent ban): the ban ends at downtime, 11 hours later. Other
/// fields are ignored. Requires `bans-manage`.
///
/// Any other category (e.g. `Account`), or an id ESI doesn't know, fails with
/// `500 ESI error: ESI returned 404`, as in the legacy waitlist.
#[utoipa::path(
    post,
    path = "/api/v2/bans",
    operation_id = "create_ban",
    tag = tags::COMMUNITY,
    security(("session" = ["bans-manage"])),
    request_body = Ban,
    responses(
        (
            status = 200,
            description = "Ban created.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`One or more body paramaters are missing: [\"id\", \"name\", \"kind\"]` (no `entity`).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn create(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(body): JsonBody<Ban>,
) -> ApiResult<&'static str> {
    account.require_access("bans-manage")?;
    let Some(entity) = &body.entity else {
        return Err(ApiError::bad_request(
            "One or more body paramaters are missing: [\"id\", \"name\", \"kind\"]",
        ));
    };

    // The legacy server asked `GET /latest/{category}s/{id}`; ESI has no such endpoint for other
    // categories ("Account"...), which surfaced as a 404 from ESI. Keep that outcome.
    let kind = EntityKind::parse(&entity.category).ok_or(EsiError::Status(404))?;
    let name = state.esi()?.entity_name(kind, entity.id).await?;

    sqlx::query(
        "INSERT INTO ban (entity_type, entity_id, entity_name, issued_at, issued_by, reason,
                          public_reason, revoked_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entity.category)
    .bind(entity.id)
    .bind(name)
    .bind(now())
    .bind(account.id)
    .bind(&body.reason)
    .bind(&body.public_reason)
    .bind(expiry(body.revoked_at))
    .execute(state.db())
    .await?;
    Ok("Ok")
}

/// Ban history of a character
///
/// Every ban ever placed on the character itself (not its corporation or alliance), active or
/// not, oldest first, including who revoked each. Requires `bans-manage`.
///
/// This path's `{id}` is a **character** id for `GET` but a **ban** id for `PATCH` and
/// `DELETE`.
#[utoipa::path(
    get,
    path = "/api/v2/bans/{id}",
    operation_id = "character_ban_history",
    tag = tags::COMMUNITY,
    security(("session" = ["bans-manage"])),
    params(("id" = i64, Path, description = "Character id.")),
    responses((status = 200, description = "The character's bans (possibly none).", body = Vec<Ban>)),
)]
async fn character_history(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(character_id): Path<i64>,
) -> ApiResult<Json<Vec<Ban>>> {
    account.require_access("bans-manage")?;
    Ok(Json(
        ban::all_bans(state.db(), character_id, "Character")
            .await?
            .unwrap_or_default(),
    ))
}

/// Edit a ban
///
/// Changes the `reason`, `public_reason` and expiry of an active ban. `revoked_at` is the expiry
/// day as for creation (11 hours are added; `null` makes the ban permanent); other fields are
/// ignored. The editor becomes the ban's issuer and `issued_at` becomes now. Requires
/// `bans-manage`.
#[utoipa::path(
    patch,
    path = "/api/v2/bans/{id}",
    operation_id = "update_ban",
    tag = tags::COMMUNITY,
    security(("session" = ["bans-manage"])),
    params(("id" = i64, Path, description = "Ban id.")),
    request_body = Ban,
    responses(
        (
            status = 200,
            description = "Ban updated.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Cannot revoke invalid ban. It is either invalid or doesn't exist` (no active ban with that id).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn update(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(ban_id): Path<i64>,
    JsonBody(body): JsonBody<Ban>,
) -> ApiResult<&'static str> {
    account.require_access("bans-manage")?;
    let now = now();
    let active: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM ban WHERE id = ? AND (revoked_at IS NULL OR revoked_at > ?)",
    )
    .bind(ban_id)
    .bind(now)
    .fetch_optional(state.db())
    .await?;
    if active.is_none() {
        return Err(ApiError::bad_request(
            "Cannot revoke invalid ban. It is either invalid or doesn't exist",
        ));
    }
    sqlx::query(
        "UPDATE ban SET reason = ?, public_reason = ?, revoked_at = ?, issued_by = ?, issued_at = ?
         WHERE id = ?",
    )
    .bind(&body.reason)
    .bind(&body.public_reason)
    .bind(expiry(body.revoked_at))
    .bind(account.id)
    .bind(now)
    .bind(ban_id)
    .execute(state.db())
    .await?;
    Ok("Ok")
}

#[derive(sqlx::FromRow)]
struct RevokeRow {
    revoked_at: Option<i64>,
    revoked_by_name: Option<String>,
}

/// Revoke a ban
///
/// Ends a ban now, recording the caller as the revoker. Requires `bans-manage`.
#[utoipa::path(
    delete,
    path = "/api/v2/bans/{id}",
    operation_id = "revoke_ban",
    tag = tags::COMMUNITY,
    security(("session" = ["bans-manage"])),
    params(("id" = i64, Path, description = "Ban id.")),
    responses(
        (
            status = 200,
            description = "Ban revoked.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Could not find a ban with the ID of <id>`, `Cannot revoke the ban as it has already expired`, or `<name> has already revoked this ban`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn revoke(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(ban_id): Path<i64>,
) -> ApiResult<&'static str> {
    account.require_access("bans-manage")?;
    let row: Option<RevokeRow> = sqlx::query_as(
        "SELECT ban.revoked_at, revoker.name AS revoked_by_name
         FROM ban LEFT JOIN `character` revoker ON ban.revoked_by = revoker.id
         WHERE ban.id = ?",
    )
    .bind(ban_id)
    .fetch_optional(state.db())
    .await?;
    let Some(row) = row else {
        return Err(ApiError::bad_request(format!(
            "Could not find a ban with the ID of {ban_id}"
        )));
    };

    let now = now();
    if row.revoked_at.is_some_and(|at| at < now) {
        return Err(ApiError::bad_request(match row.revoked_by_name {
            None => "Cannot revoke the ban as it has already expired".to_string(),
            Some(fc) => format!("{fc} has already revoked this ban"),
        }));
    }

    sqlx::query("UPDATE ban SET revoked_at = ?, revoked_by = ? WHERE id = ?")
        .bind(now)
        .bind(account.id)
        .bind(ban_id)
        .execute(state.db())
        .await?;
    Ok("Ok")
}
