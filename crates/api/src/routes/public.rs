//! Public waitlist status for logged-out visitors (an extension beyond the legacy API, where
//! the waitlist and its event stream needed a login).
//!
//! - `GET /api/public/status`: per non-archived waitlist its ID, name, open state and the number
//!   of accounts waiting. No login; `Cache-Control: public, no-cache` with a strong `ETag`
//!   (`If-None-Match` → `304`, see [`crate::cache`]).
//! - `GET /api/public/stream`: live events for anonymous clients: the `status` topic
//!   (`status_update`, `{"waitlist_id": …}`), the `incursions` topic (`incursions_update`) and
//!   the `data` topic (`data_update`, `{"file": …}`: the public fittings page refetches).
//!   `/api/incursions/stream` keeps carrying only `incursions`.

use axum::Json;
use axum::extract::State;
use axum::response::Response;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_live::topics;

use super::waitlist::waitlist_rows;
use crate::openapi::tags;
use crate::{ApiResult, AppState, cache};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(status))
        .layer(axum::middleware::from_fn(cache::revalidate))
        .routes(routes!(stream))
}

#[derive(Debug, Serialize, ToSchema)]
struct PublicStatus {
    /// Every non-archived waitlist, by ID.
    waitlists: Vec<PublicWaitlist>,
}

#[derive(Debug, Serialize, ToSchema)]
struct PublicWaitlist {
    /// Waitlist ID.
    id: i64,
    name: String,
    /// Whether pilots can x-up.
    open: bool,
    /// Accounts with an x-up on the waitlist.
    waiting: i64,
}

/// Public waitlist status
///
/// For every non-archived waitlist: its name, whether it is open and how many accounts are
/// waiting. No login needed. The answer carries `Cache-Control: public, no-cache` and a strong
/// `ETag`: caches must revalidate, and a request whose `If-None-Match` matches gets an empty
/// `304`. Clients refetch on `status_update` from `GET /api/public/stream`. An extension beyond
/// the legacy API.
#[utoipa::path(
    get,
    path = "/api/public/status",
    operation_id = "public_status",
    tag = tags::WAITLIST,
    responses(
        (
            status = 200,
            description = "The waitlists' status.",
            body = PublicStatus,
            headers(
                ("cache-control" = String, description = "`public, no-cache`"),
                ("etag" = String, description = "Strong entity tag of the body; send it as `If-None-Match` to get a `304`."),
            ),
        ),
        (status = 304, description = "Not modified: `If-None-Match` matched the current `ETag` (empty body)."),
    ),
)]
async fn status(State(state): State<AppState>) -> ApiResult<Json<PublicStatus>> {
    let waitlists = waitlist_rows(state.db(), false)
        .await?
        .into_iter()
        .map(|w| PublicWaitlist {
            id: w.id,
            name: w.name,
            open: w.is_open > 0,
            waiting: w.waiting,
        })
        .collect();
    Ok(Json(PublicStatus { waitlists }))
}

/// Public events
///
/// A public server-sent event stream of the `status` topic (`status_update`, JSON
/// `{"waitlist_id": …}`, whenever a waitlist's status may have changed: refetch
/// `GET /api/public/status`), the `incursions` topic (`incursions_update`, as on
/// `GET /api/incursions/stream`) and the `data` topic (`data_update`, JSON `{"file": …}`, when a
/// community data file changed: refetch `GET /api/fittings`) and the `wiki` topic (`wiki_update`,
/// JSON `{"slug", "section", "change"}`, when a public page or a section changed). The stream opens
/// with a `: connected` comment; keep-alive comments follow every 15 seconds. A logged-in client
/// can get these topics on `GET /api/sse/stream?public=1` instead, so it holds one stream rather
/// than two. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/public/stream",
    operation_id = "public_stream",
    tag = tags::LIVE,
    responses((status = 200, description = "The event stream.", body = String, content_type = "text/event-stream")),
)]
async fn stream(State(state): State<AppState>) -> Response {
    super::sse::event_stream(
        &state,
        topics::PUBLIC
            .iter()
            .map(|topic| topic.to_string())
            .collect(),
    )
}
