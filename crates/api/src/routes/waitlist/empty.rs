//! Emptying a waitlist.
//!
//! - `POST /api/waitlist/empty` `{waitlist_id}` (`waitlist-edit`): deletes every entry and fit of
//!   a **closed** waitlist (400 `Waitlist must be closed in order to empty it` otherwise). Text
//!   `OK`. Publishes `waitlist_update` (the legacy waitlist published nothing). An unknown or
//!   archived waitlist is a 404 `Waitlist not found` (a 500 in the legacy waitlist).

use axum::extract::State;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(empty))
}

#[derive(Deserialize, ToSchema)]
struct EmptyRequest {
    waitlist_id: i64,
}

/// Empty a waitlist
///
/// Deletes every entry and fit of a closed waitlist. Requires `waitlist-edit`. Publishes
/// `waitlist_update` and `status_update` (the legacy waitlist published nothing). An unknown or
/// archived waitlist is a 404 `Waitlist not found`.
#[utoipa::path(
    post,
    path = "/api/waitlist/empty",
    operation_id = "empty_waitlist",
    tag = tags::WAITLIST,
    security(("session" = ["waitlist-edit"])),
    request_body = EmptyRequest,
    responses(
        (status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK"),
        (status = 400, description = "`Waitlist must be closed in order to empty it`.", body = String, content_type = "text/plain"),
        (status = 404, description = "`Waitlist not found` (unknown or archived).", body = String, content_type = "text/plain"),
    ),
)]
async fn empty(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<EmptyRequest>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-edit")?;
    super::require_active(state.db(), input.waitlist_id).await?;
    let is_open: i8 = sqlx::query_scalar("SELECT is_open FROM waitlist WHERE id = ?")
        .bind(input.waitlist_id)
        .fetch_one(state.db())
        .await?;
    if is_open > 0 {
        return Err(ApiError::bad_request(
            "Waitlist must be closed in order to empty it",
        ));
    }

    let mut tx = state.db().begin().await?;
    sqlx::query(
        "DELETE FROM waitlist_entry_fit
         WHERE entry_id IN (SELECT id FROM waitlist_entry WHERE waitlist_id = ?)",
    )
    .bind(input.waitlist_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM waitlist_entry WHERE waitlist_id = ?")
        .bind(input.waitlist_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    super::notify::waitlist_update(&state, input.waitlist_id)?;
    Ok("OK")
}
