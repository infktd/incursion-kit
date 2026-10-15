//! Structured editors for the community data files (an extension beyond the legacy API): typed
//! JSON resources backed by the data files, for guided editors that never show YAML.
//!
//! - Doctrine fits (`fits.dat`), fit notes (`fitnotes.yaml`) and implant sets (`implants.yaml`)
//!   need `fits-manage` (`fc+` and above); categories, tags, rules and skill plan validation
//!   need `commanders-manage:admin`. The raw data-file editor (`/api/admin/data-files`) stays
//!   admin-only.
//! - Every write validates the request (structured errors with a field path) and then the whole
//!   data set with the new file before anything is written; then the old file is backed up, the
//!   new one written atomically, the data set swapped in and `data_update` published on the live
//!   `data` topic. `?dry_run=true` does all the checks and writes nothing.
//! - Writes answer a [`ValidationReport`]: `200` when valid (and saved, unless a dry run),
//!   `400` with the same JSON shape when not. Missing items are `404` with a text body, as
//!   elsewhere.
//! - YAML files are re-serialized: comments other than the file's leading comment block are
//!   lost (see `wl_fitting::editor`). `fits.dat` is edited in place.

mod community;
mod fits;
mod lookups;
mod notes;

use std::sync::Arc;

use axum::Json;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use wl_fitting::editor::{Edited, Rejection, data_error_fields};
use wl_fitting::{DataError, DataFile, DataSet, EditError};

use super::blocking;
use crate::{ApiError, ApiResult, AppState, ValidationReport};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(fits::routes())
        .merge(notes::routes())
        .merge(community::routes())
        .merge(lookups::routes())
}

/// Doctrine fits, fit notes and implant sets.
const FITS_MANAGE: &str = "fits-manage";
/// Everything else.
const ADMIN: &str = "commanders-manage:admin";

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct WriteQuery {
    /// `true`: check everything, write nothing.
    #[serde(default)]
    dry_run: bool,
}

/// Runs `edit` on `file` through the data store and answers the outcome.
async fn edit(
    state: &AppState,
    file: DataFile,
    dry_run: bool,
    edit: impl FnOnce(&str, &DataSet) -> Edited + Send + 'static,
) -> ApiResult<Json<ValidationReport>> {
    let store = state.data_store()?.clone();
    blocking(move || match store.edit(file, dry_run, edit) {
        Ok((validation, mut warnings)) => {
            warnings.extend(validation.warnings);
            Ok(Json(ValidationReport::valid(dry_run, warnings)))
        }
        Err(EditError::Edit(Rejection::NotFound(message))) => Err(ApiError::not_found(message)),
        Err(EditError::Edit(Rejection::Invalid(problems))) => Err(ApiError::invalid(
            dry_run,
            problems.errors,
            problems.warnings,
        )),
        Err(EditError::Data(error @ (DataError::Read { .. } | DataError::Write { .. }))) => {
            Err(ApiError::internal(error))
        }
        Err(EditError::Data(error)) => Err(ApiError::invalid(
            dry_run,
            data_error_fields(&error),
            Vec::new(),
        )),
    })
    .await
}

/// The current contents of `file` (the built-in default for an absent `rules.yaml`) with the
/// current data set.
async fn read(state: &AppState, file: DataFile) -> ApiResult<(String, Arc<DataSet>)> {
    let store = state.data_store()?.clone();
    blocking(move || {
        let content = store.read_file(file).map_err(ApiError::internal)?;
        Ok((content, store.get()))
    })
    .await
}

/// A file that does not parse (edited by hand since the last load): 500, logged.
fn unreadable(error: DataError) -> ApiError {
    ApiError::internal(error)
}
