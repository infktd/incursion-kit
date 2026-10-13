//! Module/type info lookups.
//!
//! - `GET /api/module/info?ids=<csv>` (any logged-in account): `{<type id>: {name, category,
//!   slot}}` for up to 200 ids; unknown ids are left out. 400 `Invalid type ID given` for a
//!   non-numeric id (including an empty list), 400 `Too many IDs` above 200.
//! - `GET /api/module/preload` (public): the same for every module in the doctrine fits.

use std::collections::BTreeMap;

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_eve_data::{ModuleInfo, TypeId};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(module_info))
        .routes(routes!(preload))
}

const MAX_IDS: usize = 200;

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct InfoQuery {
    /// Comma-separated type ids, at most 200 (e.g. `3170,2048`).
    ids: String,
}

/// Type information
///
/// Name, category and slot of each requested type, keyed by type id. Unknown ids are left out.
/// Requires login (any account).
#[utoipa::path(
    get,
    path = "/api/module/info",
    tag = tags::DOCTRINE,
    security(("session" = [])),
    params(InfoQuery),
    responses(
        (
            status = 200,
            description = "Type id → type information.",
            body = BTreeMap<i32, ModuleInfo>,
        ),
        (
            status = 400,
            description = "`Invalid type ID given` when an id is not a number (including an \
                           empty `ids`); `Too many IDs` for more than 200 ids.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn module_info(
    State(state): State<AppState>,
    _account: AuthenticatedAccount,
    Query(q): Query<InfoQuery>,
) -> ApiResult<Json<BTreeMap<TypeId, ModuleInfo>>> {
    let ids = q
        .ids
        .split(',')
        .map(|id| {
            id.parse::<TypeId>()
                .map_err(|_| ApiError::bad_request("Invalid type ID given"))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    if ids.len() > MAX_IDS {
        return Err(ApiError::bad_request("Too many IDs"));
    }
    Ok(Json(state.eve_data()?.module_info(ids)))
}

/// Doctrine module information
///
/// Name, category and slot of every type used by the doctrine fits (hulls, modules, charges,
/// drones...), keyed by type id, so the frontend can render fits without further lookups.
/// No login needed.
#[utoipa::path(
    get,
    path = "/api/module/preload",
    operation_id = "module_preload",
    tag = tags::DOCTRINE,
    responses((
        status = 200,
        description = "Type id → type information.",
        body = BTreeMap<i32, ModuleInfo>,
    )),
)]
async fn preload(State(state): State<AppState>) -> ApiResult<Json<BTreeMap<TypeId, ModuleInfo>>> {
    Ok(Json(state.data_store()?.get().module_preload.clone()))
}
