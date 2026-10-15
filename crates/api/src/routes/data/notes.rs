//! Fit notes (`fitnotes.yaml`) and implant sets (`implants.yaml`).

use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_fitting::editor::{self, FitNoteView};
use wl_fitting::{DataFile, FittingNote, ImplantSet};

use super::{FITS_MANAGE, WriteQuery, edit};
use crate::extract::{Path, Query};
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody, ValidationReport};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_notes, create_note))
        .routes(routes!(get_note, update_note, delete_note))
        .routes(routes!(list_sets, create_set))
        .routes(routes!(get_set, update_set, delete_set))
}

#[derive(Serialize, ToSchema)]
struct FitNoteList {
    /// In file order.
    notes: Vec<FitNoteView>,
}

/// List fit notes
///
/// Every note of `fitnotes.yaml` in file order, with whether it belongs to a doctrine fit or a
/// tier of the fits page. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/fit-notes",
    operation_id = "data_list_fit_notes",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    responses((status = 200, description = "The notes.", body = FitNoteList)),
)]
async fn list_notes(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<FitNoteList>> {
    account.require_access(FITS_MANAGE)?;
    Ok(Json(FitNoteList {
        notes: editor::fit_notes(&state.data_store()?.get()),
    }))
}

/// Get a fit note
///
/// Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/fit-notes/{name}",
    operation_id = "data_get_fit_note",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Fit or tier name.")),
    responses(
        (status = 200, description = "The note.", body = FitNoteView),
        (status = 404, description = "`Fit note not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn get_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
) -> ApiResult<Json<FitNoteView>> {
    account.require_access(FITS_MANAGE)?;
    editor::fit_notes(&state.data_store()?.get())
        .into_iter()
        .find(|note| note.name == name)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("Fit note not found"))
}

/// Create a fit note
///
/// Appends a note (Markdown) for a doctrine fit (its exact name) or a tier of the fits page.
/// Names are unique. `fitnotes.yaml` is re-serialized (its leading comment block is kept, other
/// comments are lost), backed up and written atomically; `data_update` is published.
/// `?dry_run=true` only checks. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/data/fit-notes",
    operation_id = "data_create_fit_note",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(WriteQuery),
    request_body = FittingNote,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
    ),
)]
async fn create_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(note): JsonBody<FittingNote>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::FitNotes,
        query.dry_run,
        move |content, _| editor::create_note(content, &note),
    )
    .await
}

/// Update a fit note
///
/// Replaces (and possibly renames) the note `name`. Saved like a create. Requires
/// `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/data/fit-notes/{name}",
    operation_id = "data_update_fit_note",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Current fit or tier name."), WriteQuery),
    request_body = FittingNote,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
        (status = 404, description = "`Fit note not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn update_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<WriteQuery>,
    JsonBody(note): JsonBody<FittingNote>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::FitNotes,
        query.dry_run,
        move |content, _| editor::update_note(content, &name, &note),
    )
    .await
}

/// Delete a fit note
///
/// Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    delete,
    path = "/api/data/fit-notes/{name}",
    operation_id = "data_delete_fit_note",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Fit or tier name."), WriteQuery),
    responses(
        (status = 200, description = "Valid (and deleted unless a dry run).", body = ValidationReport),
        (status = 404, description = "`Fit note not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn delete_note(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<WriteQuery>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::FitNotes,
        query.dry_run,
        move |content, _| editor::delete_note(content, &name),
    )
    .await
}

#[derive(Serialize, ToSchema)]
struct ImplantSetList {
    /// In file order.
    sets: Vec<ImplantSet>,
}

/// List implant sets
///
/// The implant guide (`implants.yaml`): every set with its slots and their default and
/// alternative implants. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/implant-sets",
    operation_id = "data_list_implant_sets",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    responses((status = 200, description = "The implant sets.", body = ImplantSetList)),
)]
async fn list_sets(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<ImplantSetList>> {
    account.require_access(FITS_MANAGE)?;
    Ok(Json(ImplantSetList {
        sets: state.data_store()?.get().implants.clone(),
    }))
}

/// Get an implant set
///
/// Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/implant-sets/{name}",
    operation_id = "data_get_implant_set",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Set name.")),
    responses(
        (status = 200, description = "The set.", body = ImplantSet),
        (status = 404, description = "`Implant set not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn get_set(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
) -> ApiResult<Json<ImplantSet>> {
    account.require_access(FITS_MANAGE)?;
    state
        .data_store()?
        .get()
        .implants
        .iter()
        .find(|set| set.name == name)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("Implant set not found"))
}

/// Create an implant set
///
/// Appends a set to the implant guide. Set names are unique; slots are 1 to 10, each at most
/// once; entry names are display text (an exact type name that is not an implant is a warning);
/// `note` and `group` are single lines. `implants.yaml` is re-serialized (its leading comment
/// block is kept), backed up and written atomically; `data_update` is published.
/// `?dry_run=true` only checks. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/data/implant-sets",
    operation_id = "data_create_implant_set",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(WriteQuery),
    request_body = ImplantSet,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths (`slots[2].default[0].name`).", body = ValidationReport),
    ),
)]
async fn create_set(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(set): JsonBody<ImplantSet>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::Implants,
        query.dry_run,
        move |content, data| editor::create_implant_set(content, data.eve(), &set),
    )
    .await
}

/// Update an implant set
///
/// Replaces (and possibly renames) the set `name`. Saved like a create. Requires
/// `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/data/implant-sets/{name}",
    operation_id = "data_update_implant_set",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Current set name."), WriteQuery),
    request_body = ImplantSet,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
        (status = 404, description = "`Implant set not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn update_set(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<WriteQuery>,
    JsonBody(set): JsonBody<ImplantSet>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::Implants,
        query.dry_run,
        move |content, data| editor::update_implant_set(content, data.eve(), &name, &set),
    )
    .await
}

/// Delete an implant set
///
/// Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    delete,
    path = "/api/data/implant-sets/{name}",
    operation_id = "data_delete_implant_set",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Set name."), WriteQuery),
    responses(
        (status = 200, description = "Valid (and deleted unless a dry run).", body = ValidationReport),
        (status = 404, description = "`Implant set not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn delete_set(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<WriteQuery>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit(
        &state,
        DataFile::Implants,
        query.dry_run,
        move |content, _| editor::delete_implant_set(content, &name),
    )
    .await
}
