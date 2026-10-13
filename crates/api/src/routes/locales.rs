//! UI translation files (`<data>/locales/`, guides under `<data>/guides/`). No login needed.
//!
//! - `GET /api/v2/locales/languages`: `{languages, labels}`, the languages that have both a
//!   `home` and a `nav` translation, with display labels from `languages.json` (else the
//!   upper-cased code).
//! - `GET /api/v2/locales/{page}/{locale}`: the flat key → string map of one page (guides read
//!   their own directory first). Missing or invalid: 400 `Locale file not found: …`.

use std::collections::BTreeMap;

use crate::extract::Path;
use axum::Json;
use axum::extract::State;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::blocking;
use crate::openapi::tags;
use crate::{ApiResult, AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_languages))
        .routes(routes!(get_locale))
}

#[derive(Serialize, ToSchema)]
struct LanguagesResponse {
    /// Language codes with both a `home` and a `nav` translation, sorted.
    languages: Vec<String>,
    /// Display label per language code: from `languages.json`, else the upper-cased code.
    labels: BTreeMap<String, String>,
}

/// Available languages
///
/// The UI languages: every language code that has both a `home` and a `nav` translation file,
/// with a display label for each (from `languages.json`, else the upper-cased code; an invalid
/// `languages.json` is ignored). No login needed.
#[utoipa::path(
    get,
    path = "/api/v2/locales/languages",
    tag = tags::CONTENT,
    responses((status = 200, description = "The languages and their labels.", body = LanguagesResponse)),
)]
async fn list_languages(State(state): State<AppState>) -> ApiResult<Json<LanguagesResponse>> {
    let content = state.content().clone();
    blocking(move || {
        let languages = content.list_valid_languages();
        let labels = content.language_labels_for(&languages);
        Ok(Json(LanguagesResponse { languages, labels }))
    })
    .await
}

/// One translation file
///
/// The flat key → string map of one page's translation (`<page>.<locale>.json`). Guide pages
/// are read from the guide's own directory first, then from the locales directory. No login
/// needed.
#[utoipa::path(
    get,
    path = "/api/v2/locales/{page}/{locale}",
    tag = tags::CONTENT,
    params(
        ("page" = String, Path, description = "Page or guide slug (`^[a-z][a-z0-9_-]{0,31}$`), e.g. `home`."),
        ("locale" = String, Path, description = "Two-letter lowercase language code, e.g. `en`."),
    ),
    responses(
        (
            status = 200,
            description = "The translation: a JSON object of strings.",
            body = BTreeMap<String, String>,
        ),
        (
            status = 400,
            description = "`Locale file not found: <page>.<locale>.json` when there is no such file; \
                           also an invalid slug or locale code (`Page slug must …`, \
                           `Locale must be a 2-letter lowercase code (e.g. en, de, fr)`) or a file \
                           that is not a valid translation (`Invalid JSON: …`, …).",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn get_locale(
    State(state): State<AppState>,
    Path((page, locale)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let content = state.content().clone();
    blocking(move || Ok(Json(content.read_locale_json(&page, &locale)?))).await
}
