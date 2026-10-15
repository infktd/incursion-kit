//! HTTP API (axum): routes, authentication, permissions and error mapping.
//!
//! See `crates/api/README.md` for the conventions route modules follow.

pub mod auth;
pub mod ban;
pub mod cache;
pub mod error;
pub mod extract;
pub mod openapi;
pub mod routes;
pub mod state;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

pub use auth::AuthenticatedAccount;
pub use error::{ApiError, ApiResult};
pub use extract::JsonBody;
pub use state::AppState;

/// Request body limit (the legacy server allowed 10 MiB for uploads).
pub const BODY_LIMIT: usize = 10 * 1024 * 1024;

/// The complete application: routes, the API docs, fallbacks and middleware.
pub fn router(state: AppState) -> Router {
    let (routes, spec) = routes::routes().split_for_parts();
    let spec = openapi::spec(spec);
    routes
        .merge(docs_routes(spec))
        // Unknown paths and unsupported methods both answer like the legacy server did.
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(TraceLayer::new_for_http().make_span_with(make_span))
                .layer(PropagateRequestIdLayer::x_request_id()),
        )
        .with_state(state)
}

/// Path of the interactive API reference.
pub const DOCS_PATH: &str = "/api/docs";
/// Path of the OpenAPI document.
pub const OPENAPI_PATH: &str = "/api/openapi.json";

/// `GET /api/docs` (interactive reference) and `GET /api/openapi.json` (the spec). They are not
/// part of the spec themselves.
fn docs_routes(spec: utoipa::openapi::OpenApi) -> Router<AppState> {
    use utoipa_scalar::{Scalar, Servable};
    let json = axum::Json(spec.clone());
    Router::new()
        .merge(Scalar::with_url(DOCS_PATH, spec).title("incursion-kit API"))
        .route(
            OPENAPI_PATH,
            axum::routing::get(move || std::future::ready(json.clone())),
        )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "404 Not Found")
}

fn make_span<B>(request: &Request<B>) -> tracing::Span {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "request",
        method = %request.method(),
        path = %request.uri().path(),
        request_id,
    )
}
