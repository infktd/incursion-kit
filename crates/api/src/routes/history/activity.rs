//! Fleet activity history (DB table `fleet_activity`).
//!
//! - `GET /api/history/fleet?character_id=` (self, own alt, or `fleet-activity-view`): the
//!   character's fleet sessions (newest first) and time per hull (most first).
//! - `GET /api/history/fleet-comp/latest` (`fleet-history-view`): `{time}`, the latest
//!   `last_seen` (now when there is no history).
//! - `GET /api/history/fleet-comp?time=` (`fleet-history-view`): every fleet member at `time`,
//!   grouped by fleet id.
//!
//! Hull names come from the EVE static data; an unknown hull fails the request with
//! `400 type error: no matches for item`, as before.

use std::collections::{BTreeMap, HashMap};

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Character, Hull, TypeId};
use wl_eve_data::{EveData, TypeError};

use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(fleet_history))
        .routes(routes!(fleet_comp_latest))
        .routes(routes!(fleet_comp))
}

/// `{id, name}` of a hull, or the legacy type error for an unknown one.
pub(super) fn hull(eve: &EveData, id: TypeId) -> Result<Hull, TypeError> {
    let name = eve.name_of(id).ok_or(TypeError::NothingMatched)?;
    Ok(Hull {
        id,
        name: name.to_string(),
    })
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character whose fleet history to show.
    character_id: i64,
}

/// One fleet session: a stretch of time in one fleet with one hull.
#[derive(Serialize, ToSchema)]
#[schema(as = FleetActivityEntry)]
struct ActivityEntry {
    hull: Hull,
    /// When the session started (`first_seen`, Unix seconds).
    logged_at: i64,
    /// Length of the session in seconds.
    time_in_fleet: i64,
}

/// Total fleet time with one hull.
#[derive(Serialize, ToSchema)]
#[schema(as = FleetActivitySummaryEntry)]
struct ActivitySummaryEntry {
    hull: Hull,
    /// Seconds, summed over every session with this hull.
    time_in_fleet: i64,
}

#[derive(Serialize, ToSchema)]
#[schema(as = FleetActivityResponse)]
struct ActivityResponse {
    /// Fleet sessions, newest first.
    activity: Vec<ActivityEntry>,
    /// Time per hull, most first (ties by hull id).
    summary: Vec<ActivitySummaryEntry>,
}

#[derive(sqlx::FromRow)]
struct ActivityRow {
    hull: i32,
    first_seen: i64,
    last_seen: i64,
}

/// Fleet activity of a character
///
/// The character's fleet sessions (newest first) and the total time per hull (most first).
/// Allowed for your own characters (main or alt), or any character with `fleet-activity-view`.
/// Hull names come from the EVE static data: a session with a hull the static data doesn't know
/// fails the whole request with `400 type error: no matches for item`.
#[utoipa::path(
    get,
    path = "/api/history/fleet",
    tag = tags::HISTORY,
    security(("session" = [])),
    params(CharacterQuery),
    responses(
        (status = 200, description = "The character's fleet activity.", body = ActivityResponse),
        (
            status = 400,
            description = "`type error: no matches for item` (a hull unknown to the static data).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn fleet_history(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<ActivityResponse>> {
    account
        .authorize_character(state.db(), query.character_id, Some("fleet-activity-view"))
        .await?;
    let eve = state.eve_data()?;
    let rows: Vec<ActivityRow> = sqlx::query_as(
        "SELECT hull, first_seen, last_seen FROM fleet_activity
         WHERE character_id = ? ORDER BY first_seen DESC, id DESC",
    )
    .bind(query.character_id)
    .fetch_all(state.db())
    .await?;

    let mut time_by_hull: HashMap<TypeId, i64> = HashMap::new();
    let mut activity = Vec::with_capacity(rows.len());
    for row in rows {
        let time_in_fleet = row.last_seen - row.first_seen;
        *time_by_hull.entry(row.hull).or_default() += time_in_fleet;
        activity.push(ActivityEntry {
            hull: hull(eve, row.hull)?,
            logged_at: row.first_seen,
            time_in_fleet,
        });
    }

    let mut summary = time_by_hull
        .into_iter()
        .map(|(id, time_in_fleet)| {
            Ok(ActivitySummaryEntry {
                hull: hull(eve, id)?,
                time_in_fleet,
            })
        })
        .collect::<Result<Vec<_>, TypeError>>()?;
    summary.sort_by(|a, b| {
        b.time_in_fleet
            .cmp(&a.time_in_fleet)
            .then(a.hull.id.cmp(&b.hull.id))
    });

    Ok(Json(ActivityResponse { activity, summary }))
}

#[derive(Serialize, ToSchema)]
struct LatestFleetTimeResponse {
    /// The latest `last_seen` of any fleet member (Unix seconds); now when there is no history.
    time: i64,
}

/// Latest fleet composition time
///
/// The most recent time any fleet member was seen, a good default for
/// `GET /api/history/fleet-comp`. Requires `fleet-history-view`.
#[utoipa::path(
    get,
    path = "/api/history/fleet-comp/latest",
    tag = tags::HISTORY,
    security(("session" = ["fleet-history-view"])),
    responses((status = 200, description = "The latest time.", body = LatestFleetTimeResponse)),
)]
async fn fleet_comp_latest(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<LatestFleetTimeResponse>> {
    account.require_access("fleet-history-view")?;
    let latest: Option<i64> = sqlx::query_scalar("SELECT MAX(last_seen) FROM fleet_activity")
        .fetch_one(state.db())
        .await?;
    Ok(Json(LatestFleetTimeResponse {
        time: latest.unwrap_or_else(|| chrono::Utc::now().timestamp()),
    }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct TimeQuery {
    /// The moment to show (Unix seconds).
    time: i64,
}

/// A fleet member at the requested time.
#[derive(Serialize, ToSchema)]
#[schema(as = FleetCompHistoryEntry)]
struct FleetCompEntry {
    hull: Hull,
    /// The pilot (`corporation_id` is always null).
    character: Character,
    /// When the pilot's session in this hull started (Unix seconds).
    logged_at: i64,
    /// Length of that whole session in seconds.
    time_in_fleet: i64,
    /// Whether the pilot was the fleet boss.
    is_boss: bool,
}

#[derive(Serialize, ToSchema)]
#[schema(as = FleetCompHistoryResponse)]
struct FleetCompResponse {
    /// Keyed by fleet id (a string in JSON).
    #[schema(value_type = BTreeMap<String, Vec<FleetCompEntry>>)]
    fleets: BTreeMap<i64, Vec<FleetCompEntry>>,
}

#[derive(sqlx::FromRow)]
struct CompRow {
    fleet_id: i64,
    hull: i32,
    first_seen: i64,
    last_seen: i64,
    character_id: i64,
    is_boss: i8,
    character_name: String,
}

/// Fleet composition at a time
///
/// Every pilot that was in a fleet at `time` (a session with `first_seen <= time <= last_seen`),
/// grouped by fleet id. Requires `fleet-history-view`. A hull unknown to the static data fails
/// the request with `400 type error: no matches for item`.
#[utoipa::path(
    get,
    path = "/api/history/fleet-comp",
    tag = tags::HISTORY,
    security(("session" = ["fleet-history-view"])),
    params(TimeQuery),
    responses(
        (status = 200, description = "The fleets at that time.", body = FleetCompResponse),
        (
            status = 400,
            description = "`type error: no matches for item` (a hull unknown to the static data).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn fleet_comp(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<TimeQuery>,
) -> ApiResult<Json<FleetCompResponse>> {
    account.require_access("fleet-history-view")?;
    let eve = state.eve_data()?;
    let rows: Vec<CompRow> = sqlx::query_as(
        "SELECT fa.fleet_id, fa.hull, fa.first_seen, fa.last_seen, fa.character_id, fa.is_boss,
                c.name AS character_name
         FROM fleet_activity fa JOIN `character` c ON fa.character_id = c.id
         WHERE fa.first_seen <= ? AND fa.last_seen >= ?
         ORDER BY fa.id",
    )
    .bind(query.time)
    .bind(query.time)
    .fetch_all(state.db())
    .await?;

    let mut fleets: BTreeMap<i64, Vec<FleetCompEntry>> = BTreeMap::new();
    for row in rows {
        fleets
            .entry(row.fleet_id)
            .or_default()
            .push(FleetCompEntry {
                hull: hull(eve, row.hull)?,
                character: Character {
                    id: row.character_id,
                    name: row.character_name,
                    corporation_id: None,
                },
                logged_at: row.first_seen,
                time_in_fleet: row.last_seen - row.first_seen,
                is_boss: row.is_boss > 0,
            });
    }
    Ok(Json(FleetCompResponse { fleets }))
}
