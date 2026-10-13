//! Badges and their assignments (DB tables `badge`, `badge_assignment`). All endpoints need
//! `badges-manage`. Badge definitions come from migrations; there is no API to create them.
//!
//! - `GET /api/badges`: every badge with its `member_count`.
//! - `GET /api/badges/{badge_id}/members`: the badge's assignments.
//! - `POST /api/badges/{badge_id}/members` `{id}`: assign (400 for an unknown badge or
//!   character, a duplicate, or while the pilot holds the badge's excluded badge); text `Ok`.
//! - `DELETE /api/badges/{badge_id}/members/{character_id}`: unassign (idempotent); text `Ok`.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_badges))
        .routes(routes!(get_badge_members, assign_badge))
        .routes(routes!(revoke_badge))
}

#[derive(Serialize, ToSchema)]
struct Badge {
    id: i64,
    name: String,
    /// Number of characters holding the badge (only in `GET /api/badges`).
    #[serde(skip_serializing_if = "Option::is_none")]
    member_count: Option<i64>,
    /// Never sent by the legacy waitlist either; kept for the wire shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_badge_id: Option<i64>,
}

#[derive(Serialize, ToSchema)]
struct BadgeCharacter {
    id: i64,
    name: String,
}

/// A badge held by a character.
#[derive(Serialize, ToSchema)]
struct BadgeAssignment {
    /// The badge (without `member_count`).
    badge: Badge,
    character: BadgeCharacter,
    /// Who assigned the badge.
    granted_by: BadgeCharacter,
    /// When the badge was assigned (Unix seconds).
    granted_at: i64,
}

/// Request body: the character id (any `name` is ignored).
#[derive(Deserialize, ToSchema)]
#[schema(as = BadgeMemberRequest)]
struct RequestCharacter {
    /// The character to assign the badge to.
    id: i64,
}

#[derive(sqlx::FromRow)]
struct BadgeRow {
    id: i64,
    name: String,
    member_count: i64,
}

#[derive(sqlx::FromRow)]
struct AssignmentRow {
    id: i64,
    name: String,
    granted_by_id: i64,
    granted_by_name: String,
    badge_id: i64,
    badge_name: String,
    granted_at: i64,
}

#[derive(sqlx::FromRow)]
struct BadgeDefinition {
    name: String,
    exclude_badge_id: Option<i64>,
}

/// List badges
///
/// Every badge with the number of characters holding it, by id. Badges are defined by
/// migrations; there is no API to create them. Requires `badges-manage`.
#[utoipa::path(
    get,
    path = "/api/badges",
    tag = tags::COMMUNITY,
    security(("session" = ["badges-manage"])),
    responses((status = 200, description = "The badges.", body = Vec<Badge>)),
)]
async fn list_badges(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<Vec<Badge>>> {
    account.require_access("badges-manage")?;
    let rows: Vec<BadgeRow> = sqlx::query_as(
        "SELECT id, name,
                (SELECT COUNT(*) FROM badge_assignment WHERE badge_assignment.badgeId = badge.id)
                    AS member_count
         FROM badge
         ORDER BY id",
    )
    .fetch_all(state.db())
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| Badge {
                id: row.id,
                name: row.name,
                member_count: Some(row.member_count),
                exclude_badge_id: None,
            })
            .collect(),
    ))
}

/// Badge members
///
/// The badge's assignments (empty for an unknown badge). Assignments whose character or granter
/// is missing from the character table are not listed. Requires `badges-manage`.
#[utoipa::path(
    get,
    path = "/api/badges/{badge_id}/members",
    tag = tags::COMMUNITY,
    security(("session" = ["badges-manage"])),
    params(("badge_id" = i64, Path, description = "Badge id.")),
    responses((status = 200, description = "The assignments.", body = Vec<BadgeAssignment>)),
)]
async fn get_badge_members(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(badge_id): Path<i64>,
) -> ApiResult<Json<Vec<BadgeAssignment>>> {
    account.require_access("badges-manage")?;
    // Inner joins: assignments without a (known) granter are not listed, as before.
    let rows: Vec<AssignmentRow> = sqlx::query_as(
        "SELECT c.id, c.name, g.id AS granted_by_id, g.name AS granted_by_name,
                b.id AS badge_id, b.name AS badge_name, ba.grantedAt AS granted_at
         FROM badge_assignment ba
         JOIN `character` c ON ba.characterId = c.id
         JOIN `character` g ON ba.grantedById = g.id
         JOIN badge b ON ba.badgeId = b.id
         WHERE ba.badgeId = ?",
    )
    .bind(badge_id)
    .fetch_all(state.db())
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| BadgeAssignment {
                badge: Badge {
                    id: row.badge_id,
                    name: row.badge_name,
                    member_count: None,
                    exclude_badge_id: None,
                },
                character: BadgeCharacter {
                    id: row.id,
                    name: row.name,
                },
                granted_by: BadgeCharacter {
                    id: row.granted_by_id,
                    name: row.granted_by_name,
                },
                granted_at: row.granted_at,
            })
            .collect(),
    ))
}

/// Assign a badge
///
/// Gives a character a badge. Refused for an unknown badge or character, when the character
/// already has the badge, or while it holds the badge's excluded badge (a badge definition can
/// name one badge it cannot be combined with). Requires `badges-manage`.
#[utoipa::path(
    post,
    path = "/api/badges/{badge_id}/members",
    tag = tags::COMMUNITY,
    security(("session" = ["badges-manage"])),
    params(("badge_id" = i64, Path, description = "Badge id.")),
    request_body = RequestCharacter,
    responses(
        (
            status = 200,
            description = "Badge assigned.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Badge not found (ID: <id>)`, `Character not found (ID: <id>)`, `<name> already has <badge> and cannot be assigned it a second time`, or `Cannot assign <badge> to <name> while they have been assigned <excluded badge>`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn assign_badge(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(badge_id): Path<i64>,
    JsonBody(request): JsonBody<RequestCharacter>,
) -> ApiResult<&'static str> {
    account.require_access("badges-manage")?;
    let db = state.db();

    let badge: BadgeDefinition =
        sqlx::query_as("SELECT name, exclude_badge_id FROM badge WHERE id = ?")
            .bind(badge_id)
            .fetch_optional(db)
            .await?
            .ok_or_else(|| ApiError::bad_request(format!("Badge not found (ID: {badge_id})")))?;

    let character_name: String = sqlx::query_scalar("SELECT name FROM `character` WHERE id = ?")
        .bind(request.id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| {
            ApiError::bad_request(format!("Character not found (ID: {})", request.id))
        })?;

    let already: Option<i64> = sqlx::query_scalar(
        "SELECT characterId FROM badge_assignment WHERE characterId = ? AND badgeId = ? LIMIT 1",
    )
    .bind(request.id)
    .bind(badge_id)
    .fetch_optional(db)
    .await?;
    if already.is_some() {
        return Err(ApiError::bad_request(format!(
            "{character_name} already has {} and cannot be assigned it a second time",
            badge.name
        )));
    }

    if let Some(exclude_id) = badge.exclude_badge_id {
        let excluded: Option<String> = sqlx::query_scalar(
            "SELECT b.name FROM badge_assignment ba JOIN badge b ON b.id = ba.badgeId
             WHERE ba.badgeId = ? AND ba.characterId = ? LIMIT 1",
        )
        .bind(exclude_id)
        .bind(request.id)
        .fetch_optional(db)
        .await?;
        if let Some(excluded) = excluded {
            return Err(ApiError::bad_request(format!(
                "Cannot assign {} to {character_name} while they have been assigned {excluded}",
                badge.name
            )));
        }
    }

    sqlx::query(
        "INSERT INTO badge_assignment (characterId, badgeId, grantedById, grantedAt)
         VALUES (?, ?, ?, ?)",
    )
    .bind(request.id)
    .bind(badge_id)
    .bind(account.id)
    .bind(chrono::Utc::now().timestamp())
    .execute(db)
    .await?;
    Ok("Ok")
}

/// Unassign a badge
///
/// Takes a badge away from a character. Idempotent: succeeds also when the character doesn't
/// hold the badge. Requires `badges-manage`.
#[utoipa::path(
    delete,
    path = "/api/badges/{badge_id}/members/{character_id}",
    tag = tags::COMMUNITY,
    security(("session" = ["badges-manage"])),
    params(
        ("badge_id" = i64, Path, description = "Badge id."),
        ("character_id" = i64, Path, description = "Character id."),
    ),
    responses((
        status = 200,
        description = "Badge unassigned (or it wasn't assigned).",
        body = String,
        content_type = "text/plain",
        example = "Ok",
    )),
)]
async fn revoke_badge(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path((badge_id, character_id)): Path<(i64, i64)>,
) -> ApiResult<&'static str> {
    account.require_access("badges-manage")?;
    sqlx::query("DELETE FROM badge_assignment WHERE characterId = ? AND badgeId = ?")
        .bind(character_id)
        .bind(badge_id)
        .execute(state.db())
        .await?;
    Ok("Ok")
}
