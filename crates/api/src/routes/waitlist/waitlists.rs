//! Waitlist administration (an extension beyond the legacy API, which had no way to add a
//! waitlist). Each waitlist is one "fleet type" (gate) with its own open state and queue.
//!
//! - `GET /api/waitlists` (`waitlist-view`): every waitlist, archived ones included.
//! - `POST /api/admin/waitlists` `{name}` (`commanders-manage:admin`): create a closed waitlist.
//! - `PATCH /api/admin/waitlists/{id}` `{name?, archived?}` (`commanders-manage:admin`): rename,
//!   archive or restore. Archiving closes the waitlist, removes its entries and unlinks its
//!   fleets; a restored waitlist comes back closed.
//!
//! Archived waitlists stay in the table (entries, history and links may reference them) but are
//! left out of the gate and public status views, and `set_open` / `empty` answer 404 for them.
//! Every change publishes `waitlist_update` and `status_update` for the waitlist.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_db::Db;

use super::notify;
use crate::extract::Path;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_waitlists))
        .routes(routes!(create_waitlist))
        .routes(routes!(update_waitlist))
}

/// The key that administers waitlists.
const ADMIN: &str = "commanders-manage:admin";
/// `waitlist.name` is a `varchar(255)`.
const MAX_NAME_CHARS: usize = 255;

/// 404 `Waitlist not found` unless the waitlist exists and isn't archived.
pub(crate) async fn require_active(db: &Db, waitlist_id: i64) -> ApiResult<()> {
    let archived: Option<i8> = sqlx::query_scalar("SELECT is_archived FROM waitlist WHERE id = ?")
        .bind(waitlist_id)
        .fetch_optional(db)
        .await?;
    match archived {
        Some(0) => Ok(()),
        _ => Err(ApiError::not_found("Waitlist not found")),
    }
}

/// A waitlist with the number of accounts waiting on it.
#[derive(sqlx::FromRow)]
pub(crate) struct WaitlistRow {
    pub id: i64,
    pub name: String,
    pub is_open: i8,
    pub is_archived: i8,
    pub waiting: i64,
}

/// Waitlists by id, with their waiting counts; archived ones only with `include_archived`.
pub(crate) async fn waitlist_rows(db: &Db, include_archived: bool) -> ApiResult<Vec<WaitlistRow>> {
    Ok(sqlx::query_as(
        "SELECT w.id, w.name, w.is_open, w.is_archived,
            (SELECT COUNT(*) FROM waitlist_entry we WHERE we.waitlist_id = w.id) AS waiting
         FROM waitlist w WHERE ? OR w.is_archived = 0 ORDER BY w.id",
    )
    .bind(include_archived)
    .fetch_all(db)
    .await?)
}

async fn waitlist_row(db: &Db, waitlist_id: i64) -> ApiResult<WaitlistInfo> {
    waitlist_rows(db, true)
        .await?
        .into_iter()
        .find(|row| row.id == waitlist_id)
        .map(WaitlistInfo::from)
        .ok_or_else(|| ApiError::not_found("Waitlist not found"))
}

/// A waitlist (fleet type).
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct WaitlistInfo {
    /// Waitlist ID (`waitlist_id` everywhere else).
    id: i64,
    name: String,
    /// Whether pilots can x-up.
    open: bool,
    /// Archived waitlists are hidden from the gate and status views and can't be opened.
    archived: bool,
    /// Accounts with an x-up on the waitlist.
    waiting: i64,
}

impl From<WaitlistRow> for WaitlistInfo {
    fn from(row: WaitlistRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            open: row.is_open > 0,
            archived: row.is_archived > 0,
            waiting: row.waiting,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct WaitlistsResponse {
    /// Every waitlist by ID, archived ones included.
    waitlists: Vec<WaitlistInfo>,
}

fn valid_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request("Invalid waitlist name"));
    }
    Ok(name.to_string())
}

/// List waitlists
///
/// Every waitlist (fleet type) with its open state, whether it is archived and how many
/// accounts are waiting. Requires `waitlist-view`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/waitlists",
    tag = tags::WAITLIST,
    security(("session" = ["waitlist-view"])),
    responses((status = 200, description = "The waitlists.", body = WaitlistsResponse)),
)]
async fn list_waitlists(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<WaitlistsResponse>> {
    account.require_access("waitlist-view")?;
    let waitlists = waitlist_rows(state.db(), true)
        .await?
        .into_iter()
        .map(WaitlistInfo::from)
        .collect();
    Ok(Json(WaitlistsResponse { waitlists }))
}

#[derive(Deserialize, ToSchema)]
struct CreateWaitlistRequest {
    /// 1 to 255 characters (surrounding whitespace is trimmed).
    name: String,
}

/// Create a waitlist
///
/// Adds a waitlist (a fleet type with its own queue), closed. Requires
/// `commanders-manage:admin`. Publishes `waitlist_update` and `status_update` for it. An
/// extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/admin/waitlists",
    tag = tags::WAITLIST,
    security(("session" = ["commanders-manage:admin"])),
    request_body = CreateWaitlistRequest,
    responses(
        (status = 200, description = "The new waitlist.", body = WaitlistInfo),
        (status = 400, description = "`Invalid waitlist name`.", body = String, content_type = "text/plain"),
    ),
)]
async fn create_waitlist(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<CreateWaitlistRequest>,
) -> ApiResult<Json<WaitlistInfo>> {
    account.require_access(ADMIN)?;
    let name = valid_name(&input.name)?;
    let id = sqlx::query("INSERT INTO waitlist (name, is_open, is_archived) VALUES (?, 0, 0)")
        .bind(&name)
        .execute(state.db())
        .await?
        .last_insert_id();
    let id = i64::try_from(id).map_err(ApiError::internal)?;
    notify::waitlist_update(&state, id)?;
    Ok(Json(waitlist_row(state.db(), id).await?))
}

#[derive(Deserialize, ToSchema)]
struct UpdateWaitlistRequest {
    /// New name: 1 to 255 characters (surrounding whitespace is trimmed).
    #[serde(default)]
    name: Option<String>,
    /// `true` archives the waitlist (closing it, removing its entries and unlinking its
    /// fleets); `false` restores an archived one, closed.
    #[serde(default)]
    archived: Option<bool>,
}

/// Rename, archive or restore a waitlist
///
/// Changes the fields given. Archiving closes the waitlist, removes every entry still on it and
/// unlinks the fleets registered for it; a restored waitlist comes back closed. Requires
/// `commanders-manage:admin`. Publishes `waitlist_update` and `status_update` for the
/// waitlist. An extension beyond the legacy API.
#[utoipa::path(
    patch,
    path = "/api/admin/waitlists/{id}",
    tag = tags::WAITLIST,
    security(("session" = ["commanders-manage:admin"])),
    params(("id" = i64, Path, description = "Waitlist ID.")),
    request_body = UpdateWaitlistRequest,
    responses(
        (status = 200, description = "The updated waitlist.", body = WaitlistInfo),
        (status = 400, description = "`Invalid waitlist name`.", body = String, content_type = "text/plain"),
        (status = 404, description = "`Waitlist not found`.", body = String, content_type = "text/plain"),
    ),
)]
async fn update_waitlist(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(waitlist_id): Path<i64>,
    JsonBody(input): JsonBody<UpdateWaitlistRequest>,
) -> ApiResult<Json<WaitlistInfo>> {
    account.require_access(ADMIN)?;
    let name = input.name.as_deref().map(valid_name).transpose()?;
    let current = waitlist_row(state.db(), waitlist_id).await?;

    let mut tx = state.db().begin().await?;
    if let Some(name) = name {
        sqlx::query("UPDATE waitlist SET name = ? WHERE id = ?")
            .bind(name)
            .bind(waitlist_id)
            .execute(&mut *tx)
            .await?;
    }
    match input.archived {
        Some(true) if !current.archived => {
            sqlx::query("UPDATE waitlist SET is_archived = 1, is_open = 0 WHERE id = ?")
                .bind(waitlist_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "DELETE FROM waitlist_entry_fit
                 WHERE entry_id IN (SELECT id FROM waitlist_entry WHERE waitlist_id = ?)",
            )
            .bind(waitlist_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM waitlist_entry WHERE waitlist_id = ?")
                .bind(waitlist_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM fleet_waitlist WHERE waitlist_id = ?")
                .bind(waitlist_id)
                .execute(&mut *tx)
                .await?;
        }
        Some(false) if current.archived => {
            sqlx::query("UPDATE waitlist SET is_archived = 0, is_open = 0 WHERE id = ?")
                .bind(waitlist_id)
                .execute(&mut *tx)
                .await?;
        }
        _ => {}
    }
    tx.commit().await?;

    notify::waitlist_update(&state, waitlist_id)?;
    Ok(Json(waitlist_row(state.db(), waitlist_id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(valid_name("  Vanguard ").unwrap(), "Vanguard");
        assert!(valid_name("   ").is_err());
        assert!(valid_name(&"é".repeat(255)).is_ok());
        assert!(valid_name(&"x".repeat(256)).is_err());
    }
}
