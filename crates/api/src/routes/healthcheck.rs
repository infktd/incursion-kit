//! `GET /healthz`: `OK` when the database answers (ESI is deliberately not checked).

use axum::extract::State;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::tags;
use crate::{ApiResult, AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(health))
}

/// Health check
///
/// `OK` when the server is up and the database answers. ESI is not checked.
#[utoipa::path(
    get,
    path = "/healthz",
    tag = tags::HEALTH,
    responses((status = 200, description = "Healthy.", body = String, content_type = "text/plain", example = "OK")),
)]
async fn health(State(state): State<AppState>) -> ApiResult<&'static str> {
    sqlx::query("SELECT 1").execute(state.db()).await?;
    Ok("OK")
}
