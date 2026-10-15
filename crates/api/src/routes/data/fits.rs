//! Doctrine fits (`fits.dat`) and EFT parsing.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_eve_data::TypeId;
use wl_fitting::DataFile;
use wl_fitting::editor::{
    self, DoctrineFitInput, DoctrineFitView, FieldError, FitLayout, FitSelector, FitWrite, TypeRef,
};

use super::{FITS_MANAGE, WriteQuery, edit_files, read};
use crate::extract::{Path, Query};
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody, ValidationReport};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get, update, delete))
        .routes(routes!(parse))
}

#[derive(Serialize, ToSchema)]
struct DoctrineFitList {
    /// Every doctrine fit, in `fits.dat` order (hidden alternatives included).
    fits: Vec<DoctrineFitView>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct FitQuery {
    /// Hull type id: only needed when several fits share the name.
    #[param(value_type = Option<i32>)]
    hull: Option<TypeId>,
    /// `true`: check everything, write nothing (writes only).
    #[serde(default)]
    dry_run: bool,
}

impl FitQuery {
    fn selector(&self, name: String) -> FitSelector {
        FitSelector {
            name,
            hull: self.hull,
        }
    }
}

/// List doctrine fits
///
/// Every doctrine fit of `fits.dat` in file order, hidden alternatives included: name, hull,
/// normalised DNA, EFT text, items by slot, the `fits.dat` section it is listed under, whether
/// the public list hides it, the waitlist category it lands in, and the implant set it uses.
/// Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/fits",
    operation_id = "data_list_fits",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    responses((status = 200, description = "The doctrine fits.", body = DoctrineFitList)),
)]
async fn list(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<DoctrineFitList>> {
    account.require_access(FITS_MANAGE)?;
    let (content, data) = read(&state, DataFile::Fits).await?;
    Ok(Json(DoctrineFitList {
        fits: editor::doctrine_fits(&content, &data),
    }))
}

/// Get a doctrine fit
///
/// One doctrine fit by name (and hull, when several fits share the name). Requires
/// `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    get,
    path = "/api/data/fits/{name}",
    operation_id = "data_get_fit",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Fit name."), FitQuery),
    responses(
        (status = 200, description = "The fit.", body = DoctrineFitView),
        (status = 400, description = "Several fits have the name and no `hull` was given (path `hull`).", body = ValidationReport),
        (status = 404, description = "`Fit not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn get(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<FitQuery>,
) -> ApiResult<Json<DoctrineFitView>> {
    account.require_access(FITS_MANAGE)?;
    let (content, data) = read(&state, DataFile::Fits).await?;
    let mut matches: Vec<DoctrineFitView> = editor::doctrine_fits(&content, &data)
        .into_iter()
        .filter(|fit| fit.name == name && query.hull.is_none_or(|hull| fit.hull.id == hull))
        .collect();
    match matches.len() {
        0 => Err(ApiError::not_found("Fit not found")),
        1 => Ok(Json(matches.remove(0))),
        _ => Err(ApiError::invalid(
            false,
            vec![FieldError {
                path: "hull".into(),
                message: format!(
                    "several fits are named {name:?}: pass the hull type id to choose one"
                ),
                line: None,
            }],
            Vec::new(),
        )),
    }
}

/// Create a doctrine fit
///
/// Adds a fitting link to `fits.dat`, given as EFT text and/or DNA (both must describe the same
/// fit). The fit goes after the last fit of `section` (a new section is appended), or after the
/// last fit. Names are unique. Everything else in `fits.dat` is left as it is. `implant_set`
/// links the fit to an implant set (stored in that set's `fits` in `implants.yaml`, which is
/// then written too). Validated (with field paths; EFT problems carry their `line`), then the
/// whole data set must load; then the old files are backed up, the new ones written atomically
/// and `data_update` published. `?dry_run=true` only checks. Requires `fits-manage`. An
/// extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/data/fits",
    operation_id = "data_create_fit",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(WriteQuery),
    request_body = DoctrineFitInput,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
    ),
)]
async fn create(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<WriteQuery>,
    JsonBody(input): JsonBody<DoctrineFitInput>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    edit_files(&state, query.dry_run, move |sources, data| {
        editor::write_fit(sources, data, FitWrite::Create(&input))
    })
    .await
}

/// Update a doctrine fit
///
/// Replaces the fit `name` (by `hull` too when several fits share the name): its link is
/// rewritten in place, renamed when `name` differs, and moved when `section` names another
/// section. Without `implant_set` the fit keeps its implant set (the link follows a rename);
/// `null` takes it out of its set, a name moves it to that set. Skill plans and fit notes refer
/// to fits by name and are not renamed with it (plans that no longer build are reported as
/// warnings). Saved like a create. Requires `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    put,
    path = "/api/data/fits/{name}",
    operation_id = "data_update_fit",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Current fit name."), FitQuery),
    request_body = DoctrineFitInput,
    responses(
        (status = 200, description = "Valid (and saved unless a dry run).", body = ValidationReport),
        (status = 400, description = "Invalid: the errors with their field paths.", body = ValidationReport),
        (status = 404, description = "`Fit not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn update(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<FitQuery>,
    JsonBody(input): JsonBody<DoctrineFitInput>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    let selector = query.selector(name);
    edit_files(&state, query.dry_run, move |sources, data| {
        editor::write_fit(sources, data, FitWrite::Update(&selector, &input))
    })
    .await
}

/// Delete a doctrine fit
///
/// Removes the fit's link from `fits.dat` (backing up the old file), and its name from its
/// implant set when no other fit has that name. Requires `fits-manage`. An extension beyond the
/// legacy API.
#[utoipa::path(
    delete,
    path = "/api/data/fits/{name}",
    operation_id = "data_delete_fit",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    params(("name" = String, Path, description = "Fit name."), FitQuery),
    responses(
        (status = 200, description = "Valid (and deleted unless a dry run).", body = ValidationReport),
        (status = 400, description = "Several fits have the name (path `hull`), or the data set does not load without the fit.", body = ValidationReport),
        (status = 404, description = "`Fit not found`", body = String, content_type = "text/plain"),
    ),
)]
async fn delete(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(name): Path<String>,
    Query(query): Query<FitQuery>,
) -> ApiResult<Json<ValidationReport>> {
    account.require_access(FITS_MANAGE)?;
    let selector = query.selector(name);
    edit_files(&state, query.dry_run, move |sources, data| {
        editor::write_fit(sources, data, FitWrite::Delete(&selector))
    })
    .await
}

#[derive(Deserialize, ToSchema)]
struct ParseRequest {
    /// One fit in EFT format, as copied from the game or a fitting tool.
    eft: String,
}

#[derive(Serialize, ToSchema)]
struct ParsedFit {
    /// The fit name of the `[Hull, Name]` header.
    name: Option<String>,
    hull: TypeRef,
    /// Ship DNA, as `fits.dat` stores it.
    dna: String,
    /// The fit as normalised EFT text.
    eft: String,
    layout: FitLayout,
    /// The waitlist category the fit would be put in.
    category: String,
    /// Doctrine fits with exactly these items.
    same_as: Vec<String>,
}

#[derive(Serialize, ToSchema)]
struct ParseResponse {
    /// Whether the text is one valid fit.
    valid: bool,
    /// The fit, when valid.
    fit: Option<ParsedFit>,
    /// Problems (path `eft`, with the `line`).
    errors: Vec<FieldError>,
    /// E.g. loaded charges that were ignored, items that go in no slot, unpublished items, as
    /// text (`line N: …`). Kept for older clients; see `warning_details`.
    warnings: Vec<String>,
    /// The same warnings structured, like `errors` (path `eft`, with the `line`).
    warning_details: Vec<FieldError>,
}

/// Parse a fit
///
/// Parses one fit in EFT format and checks it against the static data, without saving
/// anything: hull, items by slot (with names and type ids), drones and cargo, the DNA and
/// normalised EFT, the waitlist category, and every problem with its line. In-game exports are
/// accepted as they are (`/OFFLINE` markers; loaded charges are ignored with a warning, since
/// doctrine fits carry charges in the cargo). Always `200`; `valid` tells whether the fit can be
/// saved. (This path shadows a fit named `parse` for `GET`/`PUT`/`DELETE` by name.) Requires
/// `fits-manage`. An extension beyond the legacy API.
#[utoipa::path(
    post,
    path = "/api/data/fits/parse",
    operation_id = "data_parse_fit",
    tag = tags::DATA,
    security(("session" = ["fits-manage"])),
    request_body = ParseRequest,
    responses((status = 200, description = "The parsed fit and its problems.", body = ParseResponse)),
)]
async fn parse(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<ParseRequest>,
) -> ApiResult<Json<ParseResponse>> {
    account.require_access(FITS_MANAGE)?;
    let data = state.data_store()?.get();
    let eve = data.eve();
    let parsed = editor::parse_eft(&input.eft, eve, "eft");
    let fit = parsed.fit.as_ref().map(|fit| {
        let name = parsed.name.clone().unwrap_or_default();
        ParsedFit {
            hull: TypeRef {
                id: fit.hull,
                name: eve.name_of(fit.hull).unwrap_or_default().to_string(),
            },
            dna: fit.to_dna(eve).unwrap_or_default(),
            eft: editor::render_eft(fit, &name, eve),
            layout: FitLayout::of(fit, eve),
            category: data.categorize(fit).to_string(),
            same_as: data
                .fits
                .for_hull(fit.hull)
                .iter()
                .filter(|doctrine| doctrine.fit == *fit)
                .map(|doctrine| doctrine.name.clone())
                .collect(),
            name: parsed.name.clone(),
        }
    });
    Ok(Json(ParseResponse {
        valid: fit.is_some(),
        fit,
        errors: parsed.problems.errors,
        warnings: parsed
            .problems
            .warnings
            .iter()
            .map(FieldError::text)
            .collect(),
        warning_details: parsed.problems.warnings,
    }))
}
