//! Announcements (DB table `announcement`).
//!
//! - `GET /api/v2/announcements`: active announcements; no login needed.
//! - `POST /api/v2/announcements`, `PUT /api/v2/announcements/{announcement_id}`,
//!   `DELETE /api/v2/announcements/{announcement_id}` (soft delete): `waitlist-tag:HQ-FC`;
//!   text `Ok`.
//!
//! Every mutation publishes the **full active list** on the `announcments` topic, as
//! `announcment;new` (create) or `announcment;updated` (edit, delete). Editing an announcement
//! makes the editor its author, as in the legacy waitlist.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::Character;
use wl_live::{events, topics};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(update, revoke))
}

#[derive(Deserialize, ToSchema)]
#[schema(as = AnnouncementRequest)]
struct RequestPayload {
    /// The announcement text.
    message: String,
    /// Whether the frontend shows it as an alert rather than a notice.
    is_alert: bool,
    /// Free-form page filter, stored as given and interpreted by the frontend.
    pages: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = Announcement)]
struct AnnouncementPayload {
    id: i64,
    message: String,
    is_alert: bool,
    /// Omitted when not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<String>,
    /// The author, i.e. the last editor (`null` if the character no longer exists).
    created_by: Option<Character>,
    /// When the announcement was created (Unix seconds; editing doesn't change it).
    created_at: i64,
    // Never set (only active announcements are returned); kept for the wire shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    revoked_by: Option<Character>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revoked_at: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct AnnouncementRow {
    id: i64,
    message: String,
    is_alert: bool,
    pages: Option<String>,
    created_at: i64,
    creator_id: Option<i64>,
    creator_name: Option<String>,
    creator_corporation_id: Option<i64>,
}

async fn active_announcements(state: &AppState) -> ApiResult<Vec<AnnouncementPayload>> {
    let rows: Vec<AnnouncementRow> = sqlx::query_as(
        "SELECT a.id, a.message, a.is_alert, a.pages, a.created_at,
                c.id AS creator_id, c.name AS creator_name,
                c.corporation_id AS creator_corporation_id
         FROM announcement a
         LEFT JOIN `character` c ON c.id = a.created_by_id
         WHERE a.revoked_at IS NULL
         ORDER BY a.id",
    )
    .fetch_all(state.db())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| AnnouncementPayload {
            id: row.id,
            message: row.message,
            is_alert: row.is_alert,
            pages: row.pages,
            created_by: row
                .creator_id
                .zip(row.creator_name)
                .map(|(id, name)| Character {
                    id,
                    name,
                    corporation_id: row.creator_corporation_id,
                }),
            created_at: row.created_at,
            revoked_by: None,
            revoked_at: None,
        })
        .collect())
}

/// Publish the full active list to every connected client.
async fn broadcast(state: &AppState, event: &str) -> ApiResult<()> {
    let payloads = active_announcements(state).await?;
    state
        .live()
        .publish_json(topics::ANNOUNCEMENTS, event, &payloads)
        .map_err(ApiError::internal)
}

async fn is_active(state: &AppState, id: i64) -> ApiResult<bool> {
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM announcement WHERE id = ? AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(state.db())
            .await?;
    Ok(found.is_some())
}

/// Active announcements
///
/// Every announcement that hasn't been deleted, oldest first. No login needed.
#[utoipa::path(
    get,
    path = "/api/v2/announcements",
    operation_id = "list_announcements",
    tag = tags::COMMUNITY,
    responses((status = 200, description = "The active announcements.", body = Vec<AnnouncementPayload>)),
)]
async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<AnnouncementPayload>>> {
    Ok(Json(active_announcements(&state).await?))
}

/// Create an announcement
///
/// Adds an announcement authored by the caller's main. Requires `waitlist-tag:HQ-FC`.
///
/// Publishes the full list of active announcements as `announcment;new` on the `announcments`
/// topic (both spelled as in the legacy waitlist).
#[utoipa::path(
    post,
    path = "/api/v2/announcements",
    operation_id = "create_announcement",
    tag = tags::COMMUNITY,
    security(("session" = ["waitlist-tag:HQ-FC"])),
    request_body = RequestPayload,
    responses((
        status = 200,
        description = "Announcement created.",
        body = String,
        content_type = "text/plain",
        example = "Ok",
    )),
)]
async fn create(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(body): JsonBody<RequestPayload>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-tag:HQ-FC")?;
    sqlx::query(
        "INSERT INTO announcement (message, is_alert, pages, created_by_id, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&body.message)
    .bind(body.is_alert)
    .bind(&body.pages)
    .bind(account.id)
    .bind(chrono::Utc::now().timestamp())
    .execute(state.db())
    .await?;
    broadcast(&state, events::ANNOUNCEMENT_NEW).await?;
    Ok("Ok")
}

/// Edit an announcement
///
/// Replaces an active announcement's `message`, `is_alert` and `pages`. As in the legacy
/// waitlist, the editor becomes the announcement's author (`created_by`); `created_at` is kept.
/// Requires `waitlist-tag:HQ-FC`.
///
/// Publishes the full list of active announcements as `announcment;updated` on the
/// `announcments` topic.
#[utoipa::path(
    put,
    path = "/api/v2/announcements/{announcement_id}",
    operation_id = "update_announcement",
    tag = tags::COMMUNITY,
    security(("session" = ["waitlist-tag:HQ-FC"])),
    params(("announcement_id" = i64, Path, description = "Announcement id.")),
    request_body = RequestPayload,
    responses(
        (
            status = 200,
            description = "Announcement updated.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Announcment could not be found.` (unknown or deleted).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn update(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(announcement_id): Path<i64>,
    JsonBody(body): JsonBody<RequestPayload>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-tag:HQ-FC")?;
    if !is_active(&state, announcement_id).await? {
        return Err(ApiError::bad_request("Announcment could not be found."));
    }
    sqlx::query(
        "UPDATE announcement SET message = ?, is_alert = ?, pages = ?, created_by_id = ?
         WHERE id = ?",
    )
    .bind(&body.message)
    .bind(body.is_alert)
    .bind(&body.pages)
    .bind(account.id)
    .bind(announcement_id)
    .execute(state.db())
    .await?;
    broadcast(&state, events::ANNOUNCEMENT_UPDATED).await?;
    Ok("Ok")
}

/// Delete an announcement
///
/// Soft-deletes an active announcement (it is kept in the database as revoked by the caller).
/// Requires `waitlist-tag:HQ-FC`.
///
/// Publishes the full list of active announcements as `announcment;updated` on the
/// `announcments` topic.
#[utoipa::path(
    delete,
    path = "/api/v2/announcements/{announcement_id}",
    operation_id = "delete_announcement",
    tag = tags::COMMUNITY,
    security(("session" = ["waitlist-tag:HQ-FC"])),
    params(("announcement_id" = i64, Path, description = "Announcement id.")),
    responses(
        (
            status = 200,
            description = "Announcement deleted.",
            body = String,
            content_type = "text/plain",
            example = "Ok",
        ),
        (
            status = 400,
            description = "`Announcment could not be found or has already been deleted.`",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn revoke(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(announcement_id): Path<i64>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-tag:HQ-FC")?;
    if !is_active(&state, announcement_id).await? {
        return Err(ApiError::bad_request(
            "Announcment could not be found or has already been deleted.",
        ));
    }
    sqlx::query("UPDATE announcement SET revoked_by_id = ?, revoked_at = ? WHERE id = ?")
        .bind(account.id)
        .bind(chrono::Utc::now().timestamp())
        .bind(announcement_id)
        .execute(state.db())
        .await?;
    broadcast(&state, events::ANNOUNCEMENT_UPDATED).await?;
    Ok("Ok")
}
