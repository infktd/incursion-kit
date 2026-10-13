//! Waitlist categories (from `categories.yaml`, via the community data store).
//!
//! - `GET /api/categories` (any logged-in account): `{categories: [{id, name}]}` in file order.

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::WaitlistCategory;

use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(categories))
}

#[derive(Serialize, ToSchema)]
struct CategoryResponse {
    /// In `categories.yaml` order.
    categories: Vec<WaitlistCategory>,
}

/// Waitlist categories
///
/// The waitlist categories from `categories.yaml`, in file order.
/// Requires login (any account).
#[utoipa::path(
    get,
    path = "/api/categories",
    operation_id = "list_categories",
    tag = tags::DOCTRINE,
    security(("session" = [])),
    responses((status = 200, description = "The categories.", body = CategoryResponse)),
)]
async fn categories(
    State(state): State<AppState>,
    _account: AuthenticatedAccount,
) -> ApiResult<Json<CategoryResponse>> {
    let data = state.data_store()?.get();
    Ok(Json(CategoryResponse {
        categories: data.categories.categories.clone(),
    }))
}
