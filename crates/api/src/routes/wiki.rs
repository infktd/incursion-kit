//! The wiki (an extension beyond the legacy API): sections, pages, revisions, images, search.
//! The domain lives in `wl-wiki`; `docs/wiki.md` describes it for frontend authors.
//!
//! Reading needs no login for public pages. Writes need `wiki-edit` (pages, images) or
//! `wiki-admin` (sections, deleting and restoring pages, visibility of others' pages). Unlike
//! the legacy endpoints, every wiki error is JSON: `{"error": code, "message", "problems"?,
//! "current_revision"?}` with the status of [`wl_wiki::WikiError::status`]. Content updates use
//! optimistic concurrency: send the revision the edit started from (`base_revision` or
//! `If-Match`), a stale one is a `409`.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_wiki::{
    Asset, Checks, DeletedPage, Diff, ErrorBody, ErrorKind, Index, Move, NewPage, NewSection, Page,
    PageUpdate, Revision, RevisionSummary, SearchResults, Section, SectionPages, SectionUpdate,
    Status, Validation, Viewer, WikiError,
};

use crate::auth::{AuthenticatedAccount, permissions};
use crate::extract::{Path, Query};
use crate::openapi::tags;
use crate::{AppState, BODY_LIMIT};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(index))
        .routes(routes!(page_by_slug))
        .routes(routes!(create_page))
        .routes(routes!(get_page, update_page, delete_page))
        .routes(routes!(move_page))
        .routes(routes!(publish_page))
        .routes(routes!(unpublish_page))
        .routes(routes!(restore_page))
        .routes(routes!(deleted_pages))
        .routes(routes!(revisions))
        .routes(routes!(revision))
        .routes(routes!(diff))
        .routes(routes!(revert))
        .routes(routes!(list_sections, create_section))
        .routes(routes!(update_section, delete_section))
        .routes(routes!(order_sections))
        .routes(routes!(section_pages))
        .routes(routes!(search))
        .routes(routes!(validate))
        .routes(routes!(list_assets, upload_asset))
        .routes(routes!(get_asset))
        .routes(routes!(delete_asset))
}

/// A wiki error as a JSON response.
struct Failure(WikiError);

impl From<WikiError> for Failure {
    fn from(err: WikiError) -> Self {
        Self(err)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if status.is_server_error() {
            tracing::error!(error = %self.0, details = ?self.0.source, "wiki request failed");
        }
        (status, Json(self.0.body())).into_response()
    }
}

type WikiResult<T> = Result<T, Failure>;

fn viewer(account: &Option<AuthenticatedAccount>) -> Viewer {
    match account {
        Some(a) => Viewer::account(a.id, a.access),
        None => Viewer::anonymous(),
    }
}

/// A JSON body (no `Content-Type` needed, like everywhere in the API); an empty body is `{}`.
fn json<T: DeserializeOwned>(body: &[u8]) -> WikiResult<T> {
    let body = if body.iter().all(u8::is_ascii_whitespace) {
        b"{}".as_slice()
    } else {
        body
    };
    serde_json::from_slice(body).map_err(|e| {
        Failure(WikiError::new(
            ErrorKind::InvalidJson,
            format!("invalid JSON body: {e}"),
        ))
    })
}

/// What saves are checked against: the loaded doctrine fits (when the data store is wired)
/// and the permission keys roles can hold.
fn checks(state: &AppState) -> Checks {
    Checks {
        fit_names: state
            .data_store()
            .ok()
            .map(|store| store.get().fits.iter().map(|f| f.name.clone()).collect()),
        permission_keys: permissions::access_keys("admin")
            .map(|keys| keys.iter().map(|k| k.to_string()).collect())
            .unwrap_or_default(),
    }
}

/// A page with `ETag: "<revision id>"`.
fn page_response(status: StatusCode, page: Page) -> Response {
    let mut response = (status, Json(&page)).into_response();
    if let Some(revision) = page.revision_id
        && let Ok(value) = HeaderValue::from_str(&format!("\"{revision}\""))
    {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// `If-Match: "12"` (also `W/"12"` or `12`) → 12.
fn if_match(headers: &HeaderMap) -> Option<i64> {
    let value = headers.get(header::IF_MATCH)?.to_str().ok()?.trim();
    let value = value.strip_prefix("W/").unwrap_or(value);
    value.trim_matches('"').parse().ok()
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct DraftsQuery {
    /// Include drafts (only for `wiki-edit`).
    #[serde(default)]
    drafts: Option<bool>,
}

/// Wiki index
///
/// The numbered index: listed sections in order, each with its published pages in order
/// (`num` `1`, `1.1`, `1.2`…), only what the viewer may see; sections without such pages are
/// left out and the numbers close up. `drafts=true` adds drafts for editors. `can_edit` /
/// `can_admin` tell the frontend which tools to show. No login needed. An extension beyond the
/// legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki",
    operation_id = "wiki_index",
    tag = tags::WIKI,
    security((), ("session" = [])),
    params(DraftsQuery),
    responses((status = 200, description = "The index.", body = Index)),
)]
async fn index(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Query(q): Query<DraftsQuery>,
) -> WikiResult<Json<Index>> {
    Ok(Json(
        state
            .wiki()
            .index(&viewer(&account), q.drafts.unwrap_or(false))
            .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SlugQuery {
    /// The page's path, e.g. `guides/newbro`.
    slug: String,
}

/// Wiki page by path
///
/// A page with its Markdown body and what a reader shows next to it: number in the index,
/// section, updated at/by, read time, headings ("on this page") and backlinks (pages the viewer
/// may see that link to `/wiki/<slug>`). `ETag` is the current revision id. Pages the viewer
/// may not see (drafts for non-editors, gated pages) are `404`. No login needed for public
/// pages. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/page",
    operation_id = "wiki_page_by_slug",
    tag = tags::WIKI,
    security((), ("session" = [])),
    params(SlugQuery),
    responses(
        (status = 200, description = "The page.", body = Page, headers(("etag" = String, description = "`\"<revision id>\"`"))),
        (status = 404, description = "`not_found`: no such page, or not for this viewer.", body = ErrorBody),
    ),
)]
async fn page_by_slug(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Query(q): Query<SlugQuery>,
) -> WikiResult<Response> {
    let page = state
        .wiki()
        .page_by_slug(&viewer(&account), &q.slug)
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

/// Create a wiki page
///
/// Creates a page (a draft unless `status` is `published`) in a section, at `position` or at
/// the end. The body is validated (see `docs/wiki.md`): unknown directives, bad attributes and
/// unknown doctrine fit names are a `400 invalid_markdown` listing every problem. Restricting
/// visibility to a permission key requires holding it. Requires `wiki-edit`. An extension
/// beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages",
    operation_id = "wiki_create_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    request_body = NewPage,
    responses(
        (status = 201, description = "The new page.", body = Page),
        (status = 400, description = "`bad_request` (a field, an unknown section) or `invalid_markdown` (with `problems`).", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 409, description = "`conflict`: the path is taken.", body = ErrorBody),
        (status = 413, description = "`too_large`: the body is over 512 KiB.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn create_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    body: Bytes,
) -> WikiResult<Response> {
    let new: NewPage = json(&body)?;
    let page = state
        .wiki()
        .create_page(&viewer(&account), &checks(&state), new)
        .await?;
    Ok(page_response(StatusCode::CREATED, page))
}

/// Wiki page by id
///
/// The same as `GET /api/wiki/page`, by id. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/pages/{id}",
    operation_id = "wiki_get_page",
    tag = tags::WIKI,
    security((), ("session" = [])),
    params(("id" = i64, Path, description = "Page id.")),
    responses(
        (status = 200, description = "The page.", body = Page, headers(("etag" = String, description = "`\"<revision id>\"`"))),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn get_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Response> {
    let page = state.wiki().page(&viewer(&account), id).await?;
    Ok(page_response(StatusCode::OK, page))
}

/// Update a wiki page
///
/// Changes the given fields (title, summary, `for`, body, visibility, permission); absent
/// fields are kept. A content change saves a new revision (with `message`) and needs the
/// revision it is based on, as `base_revision` or `If-Match: "<revision id>"`: without one it
/// is `428 precondition_required`, a stale one `409 conflict` with `current_revision`.
/// Changing the visibility of a page someone else created needs `wiki-admin`. Requires
/// `wiki-edit`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/wiki/pages/{id}",
    operation_id = "wiki_update_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(
        ("id" = i64, Path, description = "Page id."),
        ("if-match" = Option<String>, Header, description = "`\"<revision id>\"`: the revision the edit is based on (instead of `base_revision`)."),
    ),
    request_body = PageUpdate,
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 400, description = "`bad_request` or `invalid_markdown`.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
        (status = 409, description = "`conflict`: the page changed since `base_revision` (see `current_revision`).", body = ErrorBody),
        (status = 413, description = "`too_large`.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
        (status = 428, description = "`precondition_required`: a content change without a base revision.", body = ErrorBody),
    ),
)]
async fn update_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    body: Bytes,
) -> WikiResult<Response> {
    let mut update: PageUpdate = json(&body)?;
    if update.base_revision.is_none() {
        update.base_revision = if_match(&headers);
    }
    let page = state
        .wiki()
        .update_page(&viewer(&account), &checks(&state), id, update)
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[schema(as = WikiDeleted)]
struct Deleted {
    /// The deleted page or image.
    id: i64,
}

/// Delete a wiki page
///
/// Soft-deletes a page: it disappears from reading, the index and search, keeps its revisions
/// and can be restored (`POST /api/wiki/pages/{id}/restore`); its path is free for a new page.
/// Requires `wiki-admin`. An extension beyond the legacy API.
#[utoipa::path(
    delete,
    path = "/api/wiki/pages/{id}",
    operation_id = "wiki_delete_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    params(("id" = i64, Path, description = "Page id.")),
    responses(
        (status = 200, description = "Deleted.", body = Deleted),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn delete_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Json<Deleted>> {
    state.wiki().delete_page(&viewer(&account), id).await?;
    Ok(Json(Deleted { id }))
}

/// Move a wiki page
///
/// Changes a page's path (`slug`; links to the old path are not rewritten), section and/or
/// place in its section (`position`, 1-based; the pages of the section are renumbered).
/// Requires `wiki-edit`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages/{id}/move",
    operation_id = "wiki_move_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Page id.")),
    request_body = Move,
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 400, description = "`bad_request`: an invalid path or unknown section.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
        (status = 409, description = "`conflict`: the path is taken.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn move_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
    body: Bytes,
) -> WikiResult<Response> {
    let mv: Move = json(&body)?;
    let page = state.wiki().move_page(&viewer(&account), id, mv).await?;
    Ok(page_response(StatusCode::OK, page))
}

/// Publish a wiki page
///
/// Makes a draft visible to readers (per its visibility). Requires `wiki-edit`. An extension
/// beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages/{id}/publish",
    operation_id = "wiki_publish_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Page id.")),
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn publish_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Response> {
    let page = state
        .wiki()
        .set_status(&viewer(&account), id, Status::Published)
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

/// Unpublish a wiki page
///
/// Turns a page back into a draft (only editors see it). Requires `wiki-edit`. An extension
/// beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages/{id}/unpublish",
    operation_id = "wiki_unpublish_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Page id.")),
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn unpublish_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Response> {
    let page = state
        .wiki()
        .set_status(&viewer(&account), id, Status::Draft)
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[schema(as = WikiRestore)]
struct Restore {
    /// Section to restore into (needed when the page's section was deleted); default: its own.
    #[serde(default)]
    section: Option<String>,
}

/// Restore a deleted wiki page
///
/// Brings a deleted page back, at the end of its section (or of `section`). `409 conflict`
/// when its section is gone (pass `section`) or another page now uses its path. Requires
/// `wiki-admin`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages/{id}/restore",
    operation_id = "wiki_restore_page",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    params(("id" = i64, Path, description = "Page id.")),
    request_body = Restore,
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`: no deleted page with this id.", body = ErrorBody),
        (status = 409, description = "`conflict`.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn restore_page(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
    body: Bytes,
) -> WikiResult<Response> {
    let restore: Restore = json(&body)?;
    let page = state
        .wiki()
        .restore_page(&viewer(&account), id, restore.section)
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[schema(as = WikiDeletedList)]
struct DeletedList {
    pages: Vec<DeletedPage>,
}

/// Deleted wiki pages
///
/// Deleted pages, newest first, to restore. Requires `wiki-admin`. An extension beyond the
/// legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/deleted",
    operation_id = "wiki_deleted_pages",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    responses(
        (status = 200, description = "The deleted pages.", body = DeletedList),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
    ),
)]
async fn deleted_pages(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
) -> WikiResult<Json<DeletedList>> {
    Ok(Json(DeletedList {
        pages: state.wiki().deleted_pages(&viewer(&account)).await?,
    }))
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[schema(as = WikiRevisionList)]
struct RevisionList {
    /// Newest first.
    revisions: Vec<RevisionSummary>,
}

/// Wiki page revisions
///
/// Every saved version of a page's content, newest first, without bodies. Requires
/// `wiki-edit`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/pages/{id}/revisions",
    operation_id = "wiki_revisions",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Page id.")),
    responses(
        (status = 200, description = "The revisions.", body = RevisionList),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn revisions(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Json<RevisionList>> {
    Ok(Json(RevisionList {
        revisions: state.wiki().revisions(&viewer(&account), id).await?,
    }))
}

/// Wiki page revision
///
/// One revision with its full content. Requires `wiki-edit`. An extension beyond the legacy
/// API.
#[utoipa::path(
    get,
    path = "/api/wiki/pages/{id}/revisions/{revision_id}",
    operation_id = "wiki_revision",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(
        ("id" = i64, Path, description = "Page id."),
        ("revision_id" = i64, Path, description = "Revision id."),
    ),
    responses(
        (status = 200, description = "The revision.", body = Revision),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn revision(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path((id, revision_id)): Path<(i64, i64)>,
) -> WikiResult<Json<Revision>> {
    Ok(Json(
        state
            .wiki()
            .revision(&viewer(&account), id, revision_id)
            .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct DiffQuery {
    /// The older revision.
    from: i64,
    /// The newer revision; default: the current one.
    #[serde(default)]
    to: Option<i64>,
}

/// Wiki revision diff
///
/// The line diff of the body between two revisions of a page (every line with its op and
/// line numbers, plus a unified diff), and whether the title or summary changed. Requires
/// `wiki-edit`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/pages/{id}/diff",
    operation_id = "wiki_diff",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Page id."), DiffQuery),
    responses(
        (status = 200, description = "The diff.", body = Diff),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`: page or revision.", body = ErrorBody),
    ),
)]
async fn diff(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
    Query(q): Query<DiffQuery>,
) -> WikiResult<Json<Diff>> {
    Ok(Json(
        state
            .wiki()
            .diff(&viewer(&account), id, q.from, q.to)
            .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = WikiRevert)]
struct Revert {
    /// The revision whose content becomes current again.
    revision: i64,
    /// The revision the revert is based on (or `If-Match`); default: the current one.
    #[serde(default)]
    base_revision: Option<i64>,
    /// Revision message; default "Reverted to revision N".
    #[serde(default)]
    message: Option<String>,
}

/// Revert a wiki page
///
/// Saves an old revision's content (title, summary, `for`, body) as a new revision. The
/// content is validated as on any save. Requires `wiki-edit`. An extension beyond the legacy
/// API.
#[utoipa::path(
    post,
    path = "/api/wiki/pages/{id}/revert",
    operation_id = "wiki_revert",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(
        ("id" = i64, Path, description = "Page id."),
        ("if-match" = Option<String>, Header, description = "`\"<revision id>\"` (instead of `base_revision`)."),
    ),
    request_body = Revert,
    responses(
        (status = 200, description = "The page.", body = Page),
        (status = 400, description = "`invalid_markdown`: the old content no longer validates (e.g. a fit was renamed).", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`: page or revision.", body = ErrorBody),
        (status = 409, description = "`conflict`: stale base revision.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn revert(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    body: Bytes,
) -> WikiResult<Response> {
    let revert: Revert = json(&body)?;
    let page = state
        .wiki()
        .revert(
            &viewer(&account),
            &checks(&state),
            id,
            revert.revision,
            revert.base_revision.or_else(|| if_match(&headers)),
            revert.message,
        )
        .await?;
    Ok(page_response(StatusCode::OK, page))
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[schema(as = WikiSectionList)]
struct SectionList {
    sections: Vec<Section>,
}

/// Wiki sections
///
/// Every section in order (listed or not) with the number of pages the viewer may see in
/// each. No login needed. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/sections",
    operation_id = "wiki_sections",
    tag = tags::WIKI,
    security((), ("session" = [])),
    responses((status = 200, description = "The sections.", body = SectionList)),
)]
async fn list_sections(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
) -> WikiResult<Json<SectionList>> {
    Ok(Json(SectionList {
        sections: state.wiki().sections(&viewer(&account)).await?,
    }))
}

/// Create a wiki section
///
/// Adds a section at `position` (1-based) or at the end. `listed: false` keeps it out of the
/// index (for pages shown elsewhere, like FC cheatsheets). Requires `wiki-admin`. An extension
/// beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/sections",
    operation_id = "wiki_create_section",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    request_body = NewSection,
    responses(
        (status = 200, description = "The section.", body = Section),
        (status = 400, description = "`bad_request`: an invalid key or label.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 409, description = "`conflict`: the key exists.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn create_section(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    body: Bytes,
) -> WikiResult<Json<Section>> {
    let new: NewSection = json(&body)?;
    Ok(Json(
        state.wiki().create_section(&viewer(&account), new).await?,
    ))
}

/// Update a wiki section
///
/// Renames a section or lists / unlists it. Requires `wiki-admin`. An extension beyond the
/// legacy API.
#[utoipa::path(
    put,
    path = "/api/wiki/sections/{key}",
    operation_id = "wiki_update_section",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    params(("key" = String, Path, description = "Section key.")),
    request_body = SectionUpdate,
    responses(
        (status = 200, description = "The section.", body = Section),
        (status = 400, description = "`bad_request`.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn update_section(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(key): Path<String>,
    body: Bytes,
) -> WikiResult<Json<Section>> {
    let update: SectionUpdate = json(&body)?;
    Ok(Json(
        state
            .wiki()
            .update_section(&viewer(&account), &key, update)
            .await?,
    ))
}

/// Delete a wiki section
///
/// Deletes an empty section; one with pages is `409 conflict` (move or delete them first).
/// Requires `wiki-admin`. An extension beyond the legacy API.
#[utoipa::path(
    delete,
    path = "/api/wiki/sections/{key}",
    operation_id = "wiki_delete_section",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    params(("key" = String, Path, description = "Section key.")),
    responses(
        (status = 200, description = "The remaining sections.", body = SectionList),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
        (status = 409, description = "`conflict`: the section has pages.", body = ErrorBody),
    ),
)]
async fn delete_section(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(key): Path<String>,
) -> WikiResult<Json<SectionList>> {
    let wiki = state.wiki();
    let viewer = viewer(&account);
    wiki.delete_section(&viewer, &key).await?;
    Ok(Json(SectionList {
        sections: wiki.sections(&viewer).await?,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = WikiSectionOrder)]
struct SectionOrder {
    /// Every section key, once, in the new order.
    keys: Vec<String>,
}

/// Reorder wiki sections
///
/// Puts the sections in the given order (renumbering the index). Requires `wiki-admin`. An
/// extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/sections/order",
    operation_id = "wiki_order_sections",
    tag = tags::WIKI,
    security(("session" = ["wiki-admin"])),
    request_body = SectionOrder,
    responses(
        (status = 200, description = "The sections.", body = SectionList),
        (status = 400, description = "`bad_request`: not every section exactly once.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn order_sections(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    body: Bytes,
) -> WikiResult<Json<SectionList>> {
    let order: SectionOrder = json(&body)?;
    Ok(Json(SectionList {
        sections: state
            .wiki()
            .order_sections(&viewer(&account), order.keys)
            .await?,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SectionPagesQuery {
    /// Include each page's Markdown body.
    #[serde(default)]
    body: Option<bool>,
    /// Include drafts (only for `wiki-edit`).
    #[serde(default)]
    drafts: Option<bool>,
}

/// Wiki section pages
///
/// The pages of one section in order, listed or not, optionally with their bodies: e.g. all FC
/// cheatsheets (`fc-cheatsheets`, readable with `fleet-view`) in one request. Only pages the
/// viewer may see. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/sections/{key}/pages",
    operation_id = "wiki_section_pages",
    tag = tags::WIKI,
    security((), ("session" = [])),
    params(("key" = String, Path, description = "Section key."), SectionPagesQuery),
    responses(
        (status = 200, description = "The section and its pages.", body = SectionPages),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn section_pages(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(key): Path<String>,
    Query(q): Query<SectionPagesQuery>,
) -> WikiResult<Json<SectionPages>> {
    Ok(Json(
        state
            .wiki()
            .section_pages(
                &viewer(&account),
                &key,
                q.drafts.unwrap_or(false),
                q.body.unwrap_or(false),
            )
            .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SearchQuery {
    /// The words to find (all of them; the last ones may be word beginnings).
    q: String,
    /// Most hits (1 to 50, default 20).
    #[serde(default)]
    limit: Option<usize>,
}

/// Search the wiki
///
/// Ranked full-text search over titles, headings, summaries and bodies of the pages the
/// viewer may see. Every word must match, as a whole word or a word beginning (so it works
/// while typing); short words like "FC" count. Each hit has a plain-text snippet split into
/// highlighted and plain parts. No login needed. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/search",
    operation_id = "wiki_search",
    tag = tags::WIKI,
    security((), ("session" = [])),
    params(SearchQuery),
    responses((status = 200, description = "The hits, best first.", body = SearchResults)),
)]
async fn search(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Query(q): Query<SearchQuery>,
) -> WikiResult<Json<SearchResults>> {
    Ok(Json(
        state
            .wiki()
            .search(&viewer(&account), &q.q, q.limit.unwrap_or(20))
            .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(as = WikiValidateRequest)]
struct ValidateRequest {
    body: String,
}

/// Validate a wiki page body
///
/// Checks a body as a save would, without saving: every problem with its line, plus the
/// headings, links and read time (for an editor's preview). Requires `wiki-edit`. An
/// extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/validate",
    operation_id = "wiki_validate",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    request_body = ValidateRequest,
    responses(
        (status = 200, description = "The result (`valid: false` with `problems` when it would be rejected).", body = Validation),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 422, description = "`invalid_json`.", body = ErrorBody),
    ),
)]
async fn validate(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    body: Bytes,
) -> WikiResult<Json<Validation>> {
    let viewer = viewer(&account);
    if viewer.id.is_none() {
        return Err(WikiError::unauthenticated().into());
    }
    if !viewer.can_edit() {
        return Err(WikiError::access_denied(wl_wiki::EDIT_KEY).into());
    }
    let request: ValidateRequest = json(&body)?;
    Ok(Json(wl_wiki::validate(&request.body, &checks(&state))))
}

#[derive(Debug, serde::Serialize, ToSchema)]
#[schema(as = WikiAssetList)]
struct AssetList {
    /// Newest first.
    assets: Vec<Asset>,
}

/// Wiki images
///
/// Every uploaded image, newest first. Requires `wiki-edit`. An extension beyond the legacy
/// API.
#[utoipa::path(
    get,
    path = "/api/wiki/assets",
    operation_id = "wiki_assets",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    responses(
        (status = 200, description = "The images.", body = AssetList),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
    ),
)]
async fn list_assets(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
) -> WikiResult<Json<AssetList>> {
    Ok(Json(AssetList {
        assets: state.wiki().assets(&viewer(&account)).await?,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct UploadQuery {
    /// File name with extension: `png`, `jpg`/`jpeg`, `gif`, `webp` or `svg`.
    filename: String,
}

/// Upload a wiki image
///
/// Stores the request body (the raw image, at most 10 MiB) as a new image and answers its
/// `url` for Markdown (`![alt](url)`). Images over 1 MiB are resized to 1600 px and
/// re-encoded; an opaque PNG becomes a JPEG. Requires `wiki-edit`. An extension beyond the
/// legacy API.
#[utoipa::path(
    post,
    path = "/api/wiki/assets",
    operation_id = "wiki_upload_asset",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(UploadQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "The image."),
    responses(
        (status = 200, description = "The stored image.", body = Asset),
        (status = 400, description = "`bad_request`: an unsupported type or an empty file.", body = ErrorBody),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 413, description = "`too_large`: over 10 MiB.", body = ErrorBody),
    ),
)]
async fn upload_asset(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> WikiResult<Json<Asset>> {
    let viewer = viewer(&account);
    // Check access before reading 10 MiB.
    if viewer.id.is_none() {
        return Err(WikiError::unauthenticated().into());
    }
    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| WikiError::new(ErrorKind::TooLarge, "images are at most 10 MiB"))?;
    Ok(Json(
        state
            .wiki()
            .upload_asset(&viewer, &q.filename, bytes.to_vec())
            .await?,
    ))
}

/// Wiki image
///
/// An uploaded image. Served with a long-lived immutable cache header (an id never changes
/// content) and a sandboxing `Content-Security-Policy` (SVG can't run scripts). The file name
/// in the path is informational. No login needed. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/wiki/assets/{id}/{filename}",
    operation_id = "wiki_get_asset",
    tag = tags::WIKI,
    params(
        ("id" = i64, Path, description = "Image id."),
        ("filename" = String, Path, description = "File name."),
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
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn get_asset(
    State(state): State<AppState>,
    Path((id, _filename)): Path<(i64, String)>,
) -> WikiResult<Response> {
    let (asset, bytes) = state.wiki().asset_bytes(id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, asset.content_type),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_string(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Delete a wiki image
///
/// Deletes an uploaded image; pages that show it get a broken image. Requires `wiki-edit`.
/// An extension beyond the legacy API.
#[utoipa::path(
    delete,
    path = "/api/wiki/assets/{id}",
    operation_id = "wiki_delete_asset",
    tag = tags::WIKI,
    security(("session" = ["wiki-edit"])),
    params(("id" = i64, Path, description = "Image id.")),
    responses(
        (status = 200, description = "Deleted.", body = Deleted),
        (status = 401, description = "`unauthenticated` or `access_denied`.", body = ErrorBody),
        (status = 404, description = "`not_found`.", body = ErrorBody),
    ),
)]
async fn delete_asset(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    Path(id): Path<i64>,
) -> WikiResult<Json<Deleted>> {
    state.wiki().delete_asset(&viewer(&account), id).await?;
    Ok(Json(Deleted { id }))
}
