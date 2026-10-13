//! X-up (fit) history (DB tables `fit_history`, `fitting`, `implant_set`).
//!
//! - `GET /api/history/xup?character_id=` (self, own alt, or `fit-history-view`): every fit the
//!   character x'd up with, newest first.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Hull, TypeId};

use super::activity::hull;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(xup_history))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character whose x-up history to show.
    character_id: i64,
}

/// One x-up.
#[derive(Serialize, ToSchema)]
struct XupHistoryLine {
    /// When the character x'd up (Unix seconds).
    logged_at: i64,
    /// The fit in EVE ship DNA format.
    dna: String,
    /// Implant type ids the character had at the time.
    implants: Vec<TypeId>,
    hull: Hull,
}

#[derive(Serialize, ToSchema)]
struct XupHistory {
    /// X-ups, newest first.
    xups: Vec<XupHistoryLine>,
}

#[derive(sqlx::FromRow)]
struct XupRow {
    dna: String,
    hull: i32,
    implants: String,
    logged_at: i64,
}

/// X-up history of a character
///
/// Every fit the character x'd up with, newest first, with the implants it had at the time.
/// Allowed for your own characters (main or alt), or any character with `fit-history-view`.
/// A hull unknown to the static data fails the request with `400 type error: no matches for
/// item`.
#[utoipa::path(
    get,
    path = "/api/history/xup",
    tag = tags::HISTORY,
    security(("session" = [])),
    params(CharacterQuery),
    responses(
        (status = 200, description = "The x-up history.", body = XupHistory),
        (
            status = 400,
            description = "`type error: no matches for item` (a hull unknown to the static data).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn xup_history(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<XupHistory>> {
    account
        .authorize_character(state.db(), query.character_id, Some("fit-history-view"))
        .await?;
    let eve = state.eve_data()?;
    let rows: Vec<XupRow> = sqlx::query_as(
        "SELECT fitting.dna, fitting.hull, implant_set.implants, fit_history.logged_at
         FROM fit_history
         JOIN fitting ON fit_history.fit_id = fitting.id
         JOIN implant_set ON fit_history.implant_set_id = implant_set.id
         WHERE fit_history.character_id = ?
         ORDER BY fit_history.id DESC",
    )
    .bind(query.character_id)
    .fetch_all(state.db())
    .await?;

    let mut xups = Vec::with_capacity(rows.len());
    for row in rows {
        xups.push(XupHistoryLine {
            logged_at: row.logged_at,
            // Implant sets are stored as `id:id:...`; skip anything unparsable.
            implants: row
                .implants
                .split(':')
                .filter_map(|id| id.parse().ok())
                .collect(),
            hull: hull(eve, row.hull)?,
            dna: row.dna,
        });
    }
    Ok(Json(XupHistory { xups }))
}
