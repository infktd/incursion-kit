//! Removing fits / entries (the entry's owner, or `waitlist-manage`). Both publish
//! `waitlist_update`.
//!
//! - `POST /api/waitlist/remove_fit` `{id}` (a waitlist_entry_fit id): deletes the fit, and the
//!   entry when it was its last fit. Text `OK`.
//! - `POST /api/waitlist/remove_x` `{id}` (a waitlist_entry id): deletes the entry and its fits.
//!   Text `OK`.
//!
//! Unknown ids are a 500 (`database error`), as in the legacy waitlist.

use axum::extract::State;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::notify;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(remove_fit))
        .routes(routes!(remove_x))
}

#[derive(Deserialize, ToSchema)]
struct IdRequest {
    /// The fit's x-up ID (`fits[].id`) for `remove_fit`, the entry ID (`id`) for `remove_x`.
    id: i64,
}

/// Remove a fit
///
/// Takes one fit off the waitlist, and the whole entry when it was its last fit. Allowed for
/// the entry's own account, or with `waitlist-manage`. Publishes `waitlist_update`
/// (`{"waitlist_id": …}`) on `waitlist`. An unknown fit ID is a 500 `database error`, as in the
/// legacy waitlist.
#[utoipa::path(
    post,
    path = "/api/waitlist/remove_fit",
    tag = tags::WAITLIST,
    security(("session" = [])),
    request_body = IdRequest,
    responses((status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK")),
)]
async fn remove_fit(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<IdRequest>,
) -> ApiResult<&'static str> {
    let (account_id, entry_id, waitlist_id): (i64, i64, i64) = sqlx::query_as(
        "SELECT we.account_id, wef.entry_id, we.waitlist_id FROM waitlist_entry_fit wef
         JOIN waitlist_entry we ON wef.entry_id = we.id WHERE wef.id = ?",
    )
    .bind(input.id)
    .fetch_one(state.db())
    .await?;
    account
        .authorize_character(state.db(), account_id, Some("waitlist-manage"))
        .await?;

    let mut tx = state.db().begin().await?;
    sqlx::query("DELETE FROM waitlist_entry_fit WHERE id = ?")
        .bind(input.id)
        .execute(&mut *tx)
        .await?;
    let remaining: Option<i64> =
        sqlx::query_scalar("SELECT id FROM waitlist_entry_fit WHERE entry_id = ? LIMIT 1")
            .bind(entry_id)
            .fetch_optional(&mut *tx)
            .await?;
    if remaining.is_none() {
        sqlx::query("DELETE FROM waitlist_entry WHERE id = ?")
            .bind(entry_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    notify::waitlist_update(&state, waitlist_id)?;
    Ok("OK")
}

/// Remove an entry
///
/// Takes an account's whole entry (all its fits) off the waitlist. Allowed for the entry's own
/// account, or with `waitlist-manage`. Publishes `waitlist_update` (`{"waitlist_id": …}`) on
/// `waitlist`. An unknown entry ID is a 500 `database error`, as in the legacy waitlist.
#[utoipa::path(
    post,
    path = "/api/waitlist/remove_x",
    tag = tags::WAITLIST,
    security(("session" = [])),
    request_body = IdRequest,
    responses((status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK")),
)]
async fn remove_x(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<IdRequest>,
) -> ApiResult<&'static str> {
    let (waitlist_id, account_id): (i64, i64) =
        sqlx::query_as("SELECT waitlist_id, account_id FROM waitlist_entry WHERE id = ?")
            .bind(input.id)
            .fetch_one(state.db())
            .await?;
    account
        .authorize_character(state.db(), account_id, Some("waitlist-manage"))
        .await?;

    let mut tx = state.db().begin().await?;
    sqlx::query("DELETE FROM waitlist_entry_fit WHERE entry_id = ?")
        .bind(input.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM waitlist_entry WHERE id = ?")
        .bind(input.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    notify::waitlist_update(&state, waitlist_id)?;
    Ok("OK")
}
