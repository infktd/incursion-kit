//! The public incursion tracker (`docs/incursions-api.md`). No login; responses may be cached
//! for a minute (ESI refreshes every five).
//!
//! - `GET /api/incursions`: active spawns, enriched.
//! - `GET /api/incursions/{id}`: one spawn with its full influence history and events.
//! - `GET /api/incursions/history`: ended spawns, paginated.
//! - `GET /api/incursions/log`: state changes of every spawn, newest first.
//! - `GET /api/incursions/windows`: spawn windows per security band.
//! - `GET /api/incursions/stream`: live `incursions_update` events (SSE) for anonymous clients.

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_eve_data::{SecurityBand, Universe};
use wl_incursions::rules::rfc3339;
use wl_incursions::store::{self, HistoryFilter};
use wl_incursions::view::{self, Context, HistoryPage, LogPage, SpawnView, WindowsView};
use wl_live::topics;

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState};

const CACHE_CONTROL: &str = "public, max-age=60";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(active))
        .routes(routes!(history))
        .routes(routes!(log))
        .routes(routes!(windows))
        .routes(routes!(stream))
        .routes(routes!(detail))
}

/// JSON with a public cache header.
fn cached<T: Serialize>(body: T) -> Response {
    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    response
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The enrichment context: the star map (empty when static data isn't loaded) and the
/// configured reference systems.
fn context(state: &AppState, now: i64) -> Context<'_> {
    static EMPTY: LazyLock<Universe> = LazyLock::new(Universe::default);
    let universe = state.eve_data().map_or(&*EMPTY, |eve| eve.universe());
    Context::new(
        universe,
        &state.config().incursion_tracker.reference_systems,
        now,
    )
}

#[derive(Serialize, ToSchema)]
struct ActiveResponse {
    generated_at: String,
    /// When the tracker last polled ESI (`null` before its first poll).
    last_poll_at: Option<String>,
    /// When ESI generated the data the tracker last saw.
    data_modified_at: Option<String>,
    incursions: Vec<SpawnView>,
}

/// Active incursions
///
/// Every active spawn with its systems, lifetime and jump counts from the reference systems.
#[utoipa::path(
    get,
    path = "/api/incursions",
    operation_id = "incursions_active",
    tag = tags::INCURSIONS,
    responses((
        status = 200,
        description = "Active spawns.",
        body = ActiveResponse,
        headers(("cache-control" = String, description = "`public, max-age=60`")),
    )),
)]
async fn active(State(state): State<AppState>) -> ApiResult<Response> {
    let now = now();
    let tracker = store::tracker_state(state.db()).await?;
    let incursions = view::active(state.db(), &context(&state, now)).await?;
    Ok(cached(ActiveResponse {
        generated_at: rfc3339(now),
        last_poll_at: tracker.map(|t| rfc3339(t.last_poll_at)),
        data_modified_at: tracker.and_then(|t| t.data_modified_at).map(rfc3339),
        incursions,
    }))
}

/// One incursion
///
/// A spawn (active or ended) with its full influence history and events.
#[utoipa::path(
    get,
    path = "/api/incursions/{id}",
    operation_id = "incursions_detail",
    tag = tags::INCURSIONS,
    params(("id" = i64, Path, description = "Spawn id.")),
    responses(
        (
            status = 200,
            description = "The spawn.",
            body = SpawnView,
            headers(("cache-control" = String, description = "`public, max-age=60`")),
        ),
        (status = 404, description = "`incursion not found` (also for a non-numeric id).", body = String, content_type = "text/plain"),
    ),
)]
async fn detail(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let not_found = || ApiError::not_found("incursion not found");
    let id: i64 = id.parse().map_err(|_| not_found())?;
    let spawn = view::detail(state.db(), &context(&state, now()), id)
        .await?
        .ok_or_else(not_found)?;
    Ok(cached(spawn))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct HistoryQuery {
    /// Page number, from 1 (default 1).
    page: Option<u64>,
    /// Items per page, 1 to 100 (default 20).
    per_page: Option<u64>,
    /// `highsec`, `lowsec` or `nullsec`.
    band: Option<String>,
    constellation_id: Option<i64>,
    region_id: Option<i64>,
}

fn band(value: Option<String>) -> ApiResult<Option<String>> {
    match value.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        None => Ok(None),
        Some(b) => SecurityBand::parse(b)
            .map(|band| Some(band.as_str().to_string()))
            .ok_or_else(|| ApiError::bad_request("band must be highsec, lowsec or nullsec")),
    }
}

/// Incursion history
///
/// Ended spawns, most recent first (summaries without systems or history).
#[utoipa::path(
    get,
    path = "/api/incursions/history",
    operation_id = "incursions_history",
    tag = tags::INCURSIONS,
    params(HistoryQuery),
    responses(
        (
            status = 200,
            description = "A page of ended spawns.",
            body = HistoryPage,
            headers(("cache-control" = String, description = "`public, max-age=60`")),
        ),
        (status = 400, description = "`per_page must be between 1 and 100` or `band must be highsec, lowsec or nullsec`.", body = String, content_type = "text/plain"),
    ),
)]
async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Response> {
    let per_page = query.per_page.unwrap_or(20);
    if !(1..=100).contains(&per_page) {
        return Err(ApiError::bad_request("per_page must be between 1 and 100"));
    }
    let filter = HistoryFilter {
        band: band(query.band)?,
        constellation_id: query.constellation_id,
        region_id: query.region_id,
    };
    let page = view::history(
        state.db(),
        now(),
        &filter,
        query.page.unwrap_or(1),
        per_page,
    )
    .await?;
    Ok(cached(page))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct LogQuery {
    /// Only events older than this event id (`next_before` of the previous page).
    before: Option<i64>,
    /// Events per page, 1 to 200 (default 50).
    limit: Option<u64>,
}

/// Incursion log
///
/// State changes of every spawn, newest first.
#[utoipa::path(
    get,
    path = "/api/incursions/log",
    operation_id = "incursions_log",
    tag = tags::INCURSIONS,
    params(LogQuery),
    responses(
        (
            status = 200,
            description = "A page of events.",
            body = LogPage,
            headers(("cache-control" = String, description = "`public, max-age=60`")),
        ),
        (status = 400, description = "`limit must be between 1 and 200`.", body = String, content_type = "text/plain"),
    ),
)]
async fn log(State(state): State<AppState>, Query(query): Query<LogQuery>) -> ApiResult<Response> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 200"));
    }
    Ok(cached(view::log(state.db(), query.before, limit).await?))
}

/// Spawn windows
///
/// For every security band: its slots, active spawns and the windows in which free slots
/// respawn.
#[utoipa::path(
    get,
    path = "/api/incursions/windows",
    operation_id = "incursions_windows",
    tag = tags::INCURSIONS,
    responses((
        status = 200,
        description = "Spawn windows per band.",
        body = WindowsView,
        headers(("cache-control" = String, description = "`public, max-age=60`")),
    )),
)]
async fn windows(State(state): State<AppState>) -> ApiResult<Response> {
    let slots = &state.config().incursion_tracker.slots;
    let slots = HashMap::from([
        (SecurityBand::Highsec, slots.highsec),
        (SecurityBand::Lowsec, slots.lowsec),
        (SecurityBand::Nullsec, slots.nullsec),
    ]);
    Ok(cached(view::windows(state.db(), now(), &slots).await?))
}

/// Incursion events
///
/// A public server-sent event stream of the `incursions` topic: an `incursions_update` event
/// (JSON data) whenever the tracker records a change. The stream opens with a `: connected`
/// comment; keep-alive comments follow every 15 seconds. See `docs/incursions-api.md`.
#[utoipa::path(
    get,
    path = "/api/incursions/stream",
    operation_id = "incursions_stream",
    tag = tags::INCURSIONS,
    responses((status = 200, description = "The event stream.", body = String, content_type = "text/event-stream")),
)]
async fn stream(State(state): State<AppState>) -> Response {
    super::sse::event_stream(&state, vec![topics::INCURSIONS.to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_validated() {
        assert_eq!(band(None).unwrap(), None);
        assert_eq!(band(Some(" ".into())).unwrap(), None);
        assert_eq!(
            band(Some("lowsec".into())).unwrap().as_deref(),
            Some("lowsec")
        );
        assert!(band(Some("wspace".into())).is_err());
    }
}
