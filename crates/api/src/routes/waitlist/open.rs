//! Opening / closing a waitlist.
//!
//! - `POST /api/waitlist/set_open` `{open, waitlist_id}` (`waitlist-edit`): text `OK`; publishes
//!   `waitlist_update`. An unknown or archived waitlist is a 404 `Waitlist not found` (the legacy
//!   waitlist answered `OK` and changed nothing).

use axum::extract::State;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::notify;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(set_open))
}

#[derive(Deserialize, ToSchema)]
struct SetOpenRequest {
    open: bool,
    waitlist_id: i64,
}

/// Open or close a waitlist
///
/// Sets whether pilots can x-up (closing keeps the entries). Requires `waitlist-edit`.
/// Publishes `waitlist_update` (`{"waitlist_id": …}`) on `waitlist` (and `status_update` on the
/// public `status` topic). An unknown or archived waitlist is a 404 `Waitlist not found`.
#[utoipa::path(
    post,
    path = "/api/waitlist/set_open",
    operation_id = "set_waitlist_open",
    tag = tags::WAITLIST,
    security(("session" = ["waitlist-edit"])),
    request_body = SetOpenRequest,
    responses(
        (status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK"),
        (status = 404, description = "`Waitlist not found` (unknown or archived).", body = String, content_type = "text/plain"),
    ),
)]
async fn set_open(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<SetOpenRequest>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-edit")?;
    super::require_active(state.db(), input.waitlist_id).await?;
    sqlx::query("UPDATE waitlist SET is_open = ? WHERE id = ?")
        .bind(input.open)
        .bind(input.waitlist_id)
        .execute(state.db())
        .await?;
    notify::waitlist_update(&state, input.waitlist_id)?;
    Ok("OK")
}
