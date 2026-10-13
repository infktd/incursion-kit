//! Character search.
//!
//! - `GET /api/search?query=` (`search`): characters known to the waitlist whose name contains
//!   `query` (no limit).
//! - `POST /api/search` `{search, category, strict?}` (`fleet-invite`): ESI name search as the
//!   caller's main (needs its search scope). `category` must contain `character`,
//!   `corporation` or `alliance` (comma-separated lists are allowed); the answer is the ids of
//!   the first category with results, or `[]`.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::Character;
use wl_esi::character::SearchCategory;

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(query, esi_search))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SearchQuery {
    /// Text to look for anywhere in the name (literal; `%` and `_` match themselves).
    query: String,
}

#[derive(Serialize, ToSchema)]
#[schema(as = CharacterSearchResponse)]
struct SearchResponse {
    /// The query, echoed back.
    query: String,
    /// Matching characters by name (`corporation_id` is always null).
    results: Vec<Character>,
}

#[derive(sqlx::FromRow)]
struct CharacterRow {
    id: i64,
    name: String,
}

/// Search known characters
///
/// Characters known to the waitlist whose name contains `query`, sorted by name. There is no
/// result limit. Requires `search`.
#[utoipa::path(
    get,
    path = "/api/search",
    operation_id = "search_characters",
    tag = tags::COMMUNITY,
    security(("session" = ["search"])),
    params(SearchQuery),
    responses((status = 200, description = "The matching characters.", body = SearchResponse)),
)]
async fn query(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(SearchQuery { query }): Query<SearchQuery>,
) -> ApiResult<Json<SearchResponse>> {
    account.require_access("search")?;
    let rows: Vec<CharacterRow> =
        sqlx::query_as("SELECT id, name FROM `character` WHERE name LIKE ? ORDER BY name, id")
            .bind(format!("%{}%", escape_like(&query)))
            .fetch_all(state.db())
            .await?;
    let results = rows
        .into_iter()
        .map(|row| Character {
            id: row.id,
            name: row.name,
            corporation_id: None,
        })
        .collect();
    Ok(Json(SearchResponse { query, results }))
}

#[derive(Deserialize, ToSchema)]
struct EsiSearchRequest {
    /// The name to search for (at least 3 characters).
    search: String,
    /// `character`, `corporation` or `alliance`, or a comma-separated list of them.
    category: String,
    /// Exact name match only (default `false`).
    strict: Option<bool>,
}

/// Search EVE names
///
/// An ESI name search, made as the caller's main character (which needs the ESI search scope).
/// The answer is the ids found in the first of character, corporation and alliance (in that
/// order, among the requested categories) that has results, or `[]`. Requires `fleet-invite`.
///
/// `category` is accepted if it merely contains one of the category names; unknown entries in a
/// comma-separated list are ignored. ESI failures are passed through as `ESI error: …`, with
/// ESI's status for errors it reports.
#[utoipa::path(
    post,
    path = "/api/search",
    tag = tags::COMMUNITY,
    security(("session" = ["fleet-invite"])),
    request_body = EsiSearchRequest,
    responses(
        (status = 200, description = "The ids found (possibly none).", body = Vec<i64>),
        (
            status = 400,
            description = "`Body paramater \"category\" must be one of [\"character\", \"corporation\", \"alliance\"]`, or `ESI error: search must be at least 3 characters`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn esi_search(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(request): JsonBody<EsiSearchRequest>,
) -> ApiResult<Json<Vec<i64>>> {
    // Any FC has the search scope.
    account.require_access("fleet-invite")?;
    if !["character", "corporation", "alliance"]
        .iter()
        .any(|c| request.category.contains(c))
    {
        return Err(ApiError::bad_request(
            "Body paramater \"category\" must be one of [\"character\", \"corporation\", \"alliance\"]",
        ));
    }
    let categories: Vec<SearchCategory> = request
        .category
        .split(',')
        .filter_map(SearchCategory::parse)
        .collect();

    let found = state
        .esi()?
        .search(
            account.id,
            &categories,
            &request.search,
            request.strict.unwrap_or(false),
        )
        .await?;
    let ids = [found.character, found.corporation, found.alliance]
        .into_iter()
        .find(|ids| !ids.is_empty())
        .unwrap_or_default();
    Ok(Json(ids))
}

/// Escapes `LIKE` wildcards so the search text matches literally (MySQL's escape character is `\`).
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("a_b%c\\d"), "a\\_b\\%c\\\\d");
        assert_eq!(escape_like("Pilot"), "Pilot");
    }
}
