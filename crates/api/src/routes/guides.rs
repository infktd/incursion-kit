//! Guides (`<data>/guides/<slug>/`). No login needed; a guide's `access` is advisory only (the
//! frontend hides FC guides), as in the legacy waitlist.
//!
//! - `GET /api/v2/guides`: `{guides: [{slug, title, subtitle, icon, section, access}]}`, by slug.
//! - `GET /api/v2/guides/{slug}/assets/{filename}`: a guide image, with its content type from the
//!   extension. Any failure is a 404 with a text message.

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_content::{GuideListing, asset_mime_type};

use super::blocking;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_guides))
        .routes(routes!(get_guide_asset))
}

#[derive(Serialize, ToSchema)]
struct GuidesListResponse {
    /// Sorted by slug.
    guides: Vec<GuideListing>,
}

/// List guides
///
/// Every readable guide, sorted by slug, with its title, subtitle, icon, section and access key,
/// taken from the English file (else the guide's first translation). Guides without a valid
/// file are left out. The built-in guides (`ddd`, `marauder`, `documentation`, `trainee`) are
/// listed whenever they have a file.
///
/// No login needed. A guide's `access` is advisory only: the frontend hides guides the viewer
/// lacks the key for, but their content stays public, as in the legacy waitlist.
#[utoipa::path(
    get,
    path = "/api/v2/guides",
    tag = tags::CONTENT,
    responses((status = 200, description = "The guides.", body = GuidesListResponse)),
)]
async fn list_guides(State(state): State<AppState>) -> ApiResult<Json<GuidesListResponse>> {
    let content = state.content().clone();
    blocking(move || {
        Ok(Json(GuidesListResponse {
            guides: content.list_guides(),
        }))
    })
    .await
}

/// Guide image
///
/// An image from a guide's directory, with the content type taken from its extension (`png`,
/// `jpg`/`jpeg`, `gif`, `webp` or `svg`). No login needed.
#[utoipa::path(
    get,
    path = "/api/v2/guides/{slug}/assets/{filename}",
    tag = tags::CONTENT,
    params(
        ("slug" = String, Path, description = "Guide slug."),
        ("filename" = String, Path, description = "Image file name, e.g. `fleet.png`."),
    ),
    responses(
        (
            status = 200,
            description = "The image.",
            content(
                (Vec<u8> = "image/png"),
                (Vec<u8> = "image/jpeg"),
                (Vec<u8> = "image/gif"),
                (Vec<u8> = "image/webp"),
                (Vec<u8> = "image/svg+xml"),
            ),
        ),
        (
            status = 404,
            description = "Any failure to serve the asset, with the message as text: \
                           `Guide asset not found: <filename>`, `Invalid asset filename`, \
                           `Asset must have a file extension`, `Unsupported guide asset type`, an \
                           invalid slug (`Page slug must …`), ….",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn get_guide_asset(
    State(state): State<AppState>,
    Path((slug, filename)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let content = state.content().clone();
    let result = blocking(move || {
        let bytes = content.read_asset(&slug, &filename)?;
        let mime = asset_mime_type(&filename)?;
        Ok((mime, bytes))
    })
    .await;
    match result {
        Ok((mime, bytes)) => Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response()),
        // Content errors (bad name, missing file) are 404s here, with the message.
        Err(ApiError::BadRequest(message)) => Ok((StatusCode::NOT_FOUND, message).into_response()),
        Err(other) => Err(other),
    }
}
