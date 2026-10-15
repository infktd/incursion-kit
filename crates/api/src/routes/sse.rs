//! `GET /api/sse/stream`: live events, served in-process from the [`wl_live::Hub`].
//!
//! The legacy waitlist answered with a 307 to a separate SSE server; we stream directly on the
//! same URL (`EventSource` follows redirects transparently, so clients don't notice). The topic
//! set is fixed at connect time: `announcments`, `waitlist`, `account;<account_id>`, `data`,
//! `wiki` and `wiki_restricted` (additions), plus `fleet_comp` with `fleet-view`; `?public=1`
//! adds the public stream's topics (`status`, `incursions`), so a logged-in page needs one
//! connection (over HTTP/1.1 browsers allow six per host). Each event is sent as
//! `event: <name>\ndata: <data>\n\n` (always named, never as an unnamed `message`). The stream
//! opens with a `: connected` comment, so proxies that buffer pass the response on at once, and
//! carries periodic keep-alive comments. A subscriber that falls too far behind is disconnected
//! and reconnects (see `wl_live`).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderValue;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_live::topics;

use crate::AppState;
use crate::auth::AuthenticatedAccount;
use crate::extract::Query;
use crate::openapi::tags;

/// The comment every stream starts with (`: connected`), sent as soon as it opens.
pub const OPENING_COMMENT: &str = "connected";

/// Keep-alive comment interval; well below common proxy idle timeouts.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(stream))
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct StreamQuery {
    /// `1` (or `true`): also carry the public stream's topics (`status`, `incursions`), so a
    /// logged-in client needs no second stream.
    #[serde(default, deserialize_with = "super::auth::flag")]
    #[param(value_type = Option<bool>)]
    public: bool,
}

/// The topics an account receives, plus the public stream's with `public`.
pub fn stream_topics(account: &AuthenticatedAccount, public: bool) -> Vec<String> {
    let mut topics = topics_for(account);
    if public {
        for topic in topics::PUBLIC {
            if !topics.iter().any(|t| t == topic) {
                topics.push(topic.to_owned());
            }
        }
    }
    topics
}

/// The topics an account receives.
pub fn topics_for(account: &AuthenticatedAccount) -> Vec<String> {
    let mut topics = vec![
        topics::ANNOUNCEMENTS.to_owned(),
        topics::WAITLIST.to_owned(),
        topics::account(account.id),
        topics::DATA.to_owned(),
        topics::WIKI.to_owned(),
        topics::WIKI_RESTRICTED.to_owned(),
    ];
    if account.has_access("fleet-view") {
        topics.push(topics::FLEET_COMP.to_owned());
    }
    topics
}

/// Live events
///
/// A server-sent event stream. The topics are fixed at connect time: `announcments`, `waitlist`,
/// `account;<account_id>`, `data` (community data file changes, an extension), `wiki` and
/// `wiki_restricted` (`wiki_update` on wiki changes, extensions) and, with `fleet-view`,
/// `fleet_comp`. With `?public=1` (an extension) the stream also carries the topics of
/// `GET /api/public/stream` (`status_update`, `incursions_update`), so a logged-in client holds
/// one stream instead of two (over HTTP/1.1 a browser opens at most six connections per host).
/// Each event is `event: <name>` + `data: <JSON or text>`; every event is named (none arrives
/// as an unnamed `message`). The stream opens with a `: connected` comment; keep-alive comments
/// follow every 15 seconds. A client that falls too far behind is disconnected and should
/// reconnect and refetch. See `docs/api.md` for the event catalogue.
#[utoipa::path(
    get,
    path = "/api/sse/stream",
    tag = tags::LIVE,
    security(("session" = [])),
    params(StreamQuery),
    responses((status = 200, description = "The event stream.", body = String, content_type = "text/event-stream")),
)]
async fn stream(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<StreamQuery>,
) -> Response {
    tracing::debug!(
        account_id = account.id,
        public = query.public,
        "live subscriber connecting"
    );
    event_stream(&state, stream_topics(&account, query.public))
}

/// An SSE response streaming `topics` from the live hub (shared with the public incursion
/// stream).
pub(crate) fn event_stream(state: &AppState, topics: Vec<String>) -> Response {
    let subscription = state.live().subscribe(topics);
    tracing::debug!(subscriber = subscription.id(), "live subscriber connected");
    // An opening comment flushes the response headers through buffering proxies right away;
    // otherwise nothing arrives until the first event or keep-alive.
    let opened = futures::stream::once(std::future::ready(Ok::<_, Infallible>(
        Event::default().comment(OPENING_COMMENT),
    )));
    let events =
        opened.chain(subscription.map(|event| {
            Ok::<_, Infallible>(Event::default().event(&event.name).data(&event.data))
        }));
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    // Ask nginx-style proxies not to buffer the stream.
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}
