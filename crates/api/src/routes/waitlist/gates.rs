//! Gate data: every active waitlist at a glance, for any logged-in pilot (an extension beyond
//! the legacy API).
//!
//! - `GET /api/waitlist/gates`: per non-archived waitlist its open state, the number of accounts
//!   waiting, the bosses of the fleets registered for it (`waitlist_id` of
//!   `POST /api/fleet/register`) and the number of fits queued per category. No pilot
//!   identities. Clients refetch on `waitlist_update`.

use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::Character;

use super::waitlists::waitlist_rows;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(gates))
}

#[derive(Debug, Serialize, ToSchema)]
struct GatesResponse {
    /// Every non-archived waitlist, by ID.
    gates: Vec<Gate>,
}

#[derive(Debug, Serialize, ToSchema)]
struct Gate {
    /// Waitlist ID.
    id: i64,
    name: String,
    /// Whether pilots can x-up.
    open: bool,
    /// Accounts with an x-up on the waitlist.
    waiting: i64,
    /// The bosses of the fleets registered for this waitlist, by fleet registration
    /// (`corporation_id` is always absent). Empty when no fleet is linked.
    fcs: Vec<Character>,
    /// Fits queued per waitlist category, in configured order (every category, zeros
    /// included).
    queued: Vec<QueuedCount>,
}

#[derive(Debug, Serialize, ToSchema)]
struct QueuedCount {
    /// Category ID (`categories.yaml`; the keys of fleet registration `assignments`).
    id: String,
    /// Category name (as in `GET /api/waitlist`).
    name: String,
    /// Fits in this category on the waitlist.
    count: i64,
}

/// Gates (waitlist overview)
///
/// For every non-archived waitlist: its name, whether it is open, how many accounts are
/// waiting, the bosses of the fleets registered for it, and the number of fits queued per
/// category. Any logged-in account may call it; it names no waiting pilot. Clients refetch on
/// `waitlist_update`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/waitlist/gates",
    operation_id = "waitlist_gates",
    tag = tags::WAITLIST,
    security(("session" = [])),
    responses((status = 200, description = "The gates.", body = GatesResponse)),
)]
async fn gates(
    State(state): State<AppState>,
    _account: AuthenticatedAccount,
) -> ApiResult<Json<GatesResponse>> {
    let data = state.data_store()?.get();
    let waitlists = waitlist_rows(state.db(), false).await?;

    let mut fcs: HashMap<i64, Vec<Character>> = HashMap::new();
    let bosses: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT fw.waitlist_id, `character`.id, `character`.name FROM fleet_waitlist fw
         JOIN fleet ON fleet.id = fw.fleet_id
         JOIN `character` ON `character`.id = fleet.boss_id
         ORDER BY fw.fleet_id",
    )
    .fetch_all(state.db())
    .await?;
    for (waitlist_id, id, name) in bosses {
        fcs.entry(waitlist_id).or_default().push(Character {
            id,
            name,
            corporation_id: None,
        });
    }

    let counts: HashMap<(i64, String), i64> = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT we.waitlist_id, wef.category, COUNT(*) FROM waitlist_entry_fit wef
         JOIN waitlist_entry we ON we.id = wef.entry_id
         GROUP BY we.waitlist_id, wef.category",
    )
    .fetch_all(state.db())
    .await?
    .into_iter()
    .map(|(waitlist_id, category, count)| ((waitlist_id, category), count))
    .collect();

    let gates = waitlists
        .into_iter()
        .map(|w| Gate {
            queued: data
                .categories
                .categories
                .iter()
                .map(|c| QueuedCount {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    count: counts.get(&(w.id, c.id.clone())).copied().unwrap_or(0),
                })
                .collect(),
            fcs: fcs.remove(&w.id).unwrap_or_default(),
            id: w.id,
            name: w.name,
            open: w.is_open > 0,
            waiting: w.waiting,
        })
        .collect();
    Ok(Json(GatesResponse { gates }))
}
