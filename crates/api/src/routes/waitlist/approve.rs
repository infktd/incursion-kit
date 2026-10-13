//! Approving / rejecting fits (`waitlist-manage`). Both publish `waitlist_update`.
//!
//! - `POST /api/waitlist/approve` `{id}` (a waitlist_entry_fit id): text `OK`.
//! - `POST /api/waitlist/reject` `{id, review_comment}`: text `OK`.
//!
//! An unknown fit id is a 500 (`database error`), as in the legacy waitlist.

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
        .routes(routes!(approve))
        .routes(routes!(reject))
}

/// The waitlist of a fit (`RowNotFound`, i.e. 500, if there is no such fit).
pub(super) async fn waitlist_of_fit(state: &AppState, fit_id: i64) -> ApiResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT we.waitlist_id FROM waitlist_entry_fit wef
         JOIN waitlist_entry we ON we.id = wef.entry_id WHERE wef.id = ?",
    )
    .bind(fit_id)
    .fetch_one(state.db())
    .await?)
}

#[derive(Deserialize, ToSchema)]
#[schema(as = ApproveFitRequest)]
struct ApproveRequest {
    /// The fit's x-up ID (`fits[].id` in `GET /api/waitlist`).
    id: i64,
}

/// Approve a fit
///
/// Marks an x-up'd fit as approved. Requires `waitlist-manage`. Publishes `waitlist_update`
/// (`{"waitlist_id": …}`) on `waitlist`. An unknown fit ID is a 500 `database error`, as in
/// the legacy waitlist.
#[utoipa::path(
    post,
    path = "/api/waitlist/approve",
    operation_id = "approve_fit",
    tag = tags::WAITLIST,
    security(("session" = ["waitlist-manage"])),
    request_body = ApproveRequest,
    responses((status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK")),
)]
async fn approve(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<ApproveRequest>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-manage")?;
    let waitlist_id = waitlist_of_fit(&state, input.id).await?;
    sqlx::query("UPDATE waitlist_entry_fit SET approved = 1 WHERE id = ?")
        .bind(input.id)
        .execute(state.db())
        .await?;
    notify::waitlist_update(&state, waitlist_id)?;
    Ok("OK")
}

#[derive(Deserialize, ToSchema)]
struct RejectRequest {
    /// The fit's x-up ID (`fits[].id` in `GET /api/waitlist`).
    id: i64,
    /// Why the fit was rejected, shown to the pilot.
    review_comment: String,
}

/// Reject a fit
///
/// Marks an x-up'd fit as not approved and stores the review comment (the fit stays on the
/// waitlist). Requires `waitlist-manage`. Publishes `waitlist_update` (`{"waitlist_id": …}`) on
/// `waitlist`. An unknown fit ID is a 500 `database error`, as in the legacy waitlist.
#[utoipa::path(
    post,
    path = "/api/waitlist/reject",
    operation_id = "reject_fit",
    tag = tags::WAITLIST,
    security(("session" = ["waitlist-manage"])),
    request_body = RejectRequest,
    responses((status = 200, description = "Done.", body = String, content_type = "text/plain", example = "OK")),
)]
async fn reject(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<RejectRequest>,
) -> ApiResult<&'static str> {
    account.require_access("waitlist-manage")?;
    let waitlist_id = waitlist_of_fit(&state, input.id).await?;
    sqlx::query("UPDATE waitlist_entry_fit SET approved = 0, review_comment = ? WHERE id = ?")
        .bind(&input.review_comment)
        .bind(input.id)
        .execute(state.db())
        .await?;
    notify::waitlist_update(&state, waitlist_id)?;
    Ok("OK")
}
