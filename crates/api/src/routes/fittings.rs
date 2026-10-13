//! Doctrine fittings and notes.
//!
//! - `GET /api/fittings` (public): `{fittingdata: [{name, dna}], notes, implants, rules}`: the
//!   doctrine fits (without hidden alternatives), fit notes, the implant guide and the type ids
//!   that put a fit in the logistics category.

use axum::Json;
use axum::extract::State;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_fitting::FittingsResponse;

use crate::openapi::tags;
use crate::{ApiResult, AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(fittings))
}

/// Doctrine fittings
///
/// Everything the public fittings page shows: the doctrine fits as ship DNA (fits whose name
/// contains the hidden-fit marker of `rules.yaml`, `ALTERNATIVE` by default, are left out), the
/// fit notes (`fitnotes.yaml`), the implant guide (`implants.yaml`), and in `rules` the type ids
/// that put a fit in the configured category (logistics by default). Every field is always
/// present. No login needed.
#[utoipa::path(
    get,
    path = "/api/fittings",
    operation_id = "list_fittings",
    tag = tags::DOCTRINE,
    responses((status = 200, description = "The doctrine fits and notes.", body = FittingsResponse)),
)]
async fn fittings(State(state): State<AppState>) -> ApiResult<Json<FittingsResponse>> {
    Ok(Json(state.data_store()?.get().fittings.clone()))
}
