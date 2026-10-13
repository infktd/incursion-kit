//! Admin editor for the data directory: community data files, translations, guides and guide
//! images. Every endpoint needs `commanders-manage:admin`; failures are 400 with a text message.
//!
//! - `GET /api/admin/data-files`: `{files: [{name, file_type, requires_reload, size, is_guide}]}`
//!   sorted by name: the community data files present (`skills.yaml`, `categories.yaml`,
//!   `modules.yaml`, `tags.yaml`, `fits.dat`, `fitnotes.yaml`, `implants.yaml`,
//!   `skillplan.yaml`), `languages.json`, and every translation and guide file.
//! - `GET /api/admin/data-files/{filename}`: the raw file as text.
//! - `POST /api/admin/data-files/{filename}?kind=` (raw text body): replace a file. Community data
//!   files are validated against the whole data set **before** anything is written (the legacy
//!   waitlist wrote first), then backed up, written atomically and loaded. `kind=guide` saves a
//!   `<slug>.<locale>.json` as a guide (creating the guide). Text `… saved successfully`.
//! - `DELETE /api/admin/data-files/{filename}`: delete a translation or guide file (backed up).
//! - `POST /api/admin/data-files/{filename}/reload`: reload the community data from disk (only
//!   for the files the legacy waitlist cached: skills, categories, modules, tags, fits).
//! - `GET /api/admin/guides/{slug}/assets`: `{assets: [{name, size}]}`.
//! - `POST /api/admin/guides/{slug}/assets/{filename}` (raw bytes): upload a guide image; large
//!   ones are shrunk, and an opaque PNG is saved as `.jpg`. `{filename, size}` (the saved name).
//! - `DELETE /api/admin/guides/{slug}/assets/{filename}`: text `Asset deleted successfully`.

use crate::extract::{Path, Query};
use axum::Json;
use axum::body::Body;
use axum::extract::State;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_content::{GuideAssetInfo, LANGUAGE_LABELS_FILENAME, parse_admin_filename};
use wl_fitting::{DataError, DataFile};

use super::blocking;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_data_files))
        .routes(routes!(get_data_file, save_data_file, delete_data_file))
        .routes(routes!(reload_data_file))
        .routes(routes!(list_guide_assets))
        .routes(routes!(upload_guide_asset, delete_guide_asset))
}

const ADMIN: &str = "commanders-manage:admin";

/// Largest accepted image upload (the legacy server read at most 10 MB, decimal).
const MAX_UPLOAD_BYTES: usize = 10_000_000;

/// A community data file the admin editor may edit.
fn editable_data_file(filename: &str) -> Option<DataFile> {
    DataFile::from_file_name(filename).filter(|file| file.legacy_editable())
}

fn check_filename(filename: &str) -> ApiResult<()> {
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
        return Err(ApiError::bad_request("Invalid filename"));
    }
    Ok(())
}

fn data_error(err: DataError) -> ApiError {
    ApiError::BadRequest(err.to_string())
}

#[derive(Serialize, ToSchema)]
struct DataFileInfo {
    /// File name, e.g. `skills.yaml` or `home.en.json`.
    name: String,
    /// `YAML`, `DAT`, `Language labels`, `Locale JSON` or `Guide JSON`.
    file_type: String,
    /// Whether the legacy waitlist needed a reload after editing the file (skills, categories,
    /// modules, tags and fits). Every save here takes effect immediately; the flag is kept for
    /// the editor.
    requires_reload: bool,
    /// Size in bytes.
    size: u64,
    /// Whether the file is a guide translation.
    is_guide: bool,
}

#[derive(Serialize, ToSchema)]
struct DataFilesListResponse {
    /// Sorted by name.
    files: Vec<DataFileInfo>,
}

/// List data files (admin)
///
/// The files the admin editor can edit, sorted by name: the community data files present in the
/// data directory (`skills.yaml`, `categories.yaml`, `modules.yaml`, `tags.yaml`, `fits.dat`,
/// `fitnotes.yaml`, `implants.yaml`, `skillplan.yaml`), `languages.json` if it exists, and
/// every translation and guide file (`<page>.<locale>.json`). Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/data-files",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    responses(
        (status = 200, description = "The files.", body = DataFilesListResponse),
        (status = 400, description = "`Failed to read data directory: …` / `Failed to read directory entry: …`", body = String, content_type = "text/plain"),
    ),
)]
async fn list_data_files(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<DataFilesListResponse>> {
    account.require_access(ADMIN)?;
    let content = state.content().clone();
    blocking(move || {
        let mut files = Vec::new();
        let entries = std::fs::read_dir(content.dir())
            .map_err(|e| ApiError::bad_request(format!("Failed to read data directory: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                ApiError::bad_request(format!("Failed to read directory entry: {e}"))
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(file) = editable_data_file(&name) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let file_type = if name.ends_with(".yaml") {
                "YAML"
            } else if name.ends_with(".dat") {
                "DAT"
            } else {
                "Unknown"
            };
            files.push(DataFileInfo {
                name,
                file_type: file_type.to_string(),
                requires_reload: file.legacy_requires_reload(),
                size: metadata.len(),
                is_guide: false,
            });
        }

        if let Some(size) = content.language_labels_size() {
            files.push(DataFileInfo {
                name: LANGUAGE_LABELS_FILENAME.to_string(),
                file_type: "Language labels".to_string(),
                requires_reload: false,
                size,
                is_guide: false,
            });
        }

        for name in content.list_admin_filenames() {
            let size = content.admin_file_size(&name).unwrap_or(0);
            let is_guide =
                parse_admin_filename(&name).is_some_and(|(page, _)| content.is_guide_page(&page));
            files.push(DataFileInfo {
                name,
                file_type: if is_guide {
                    "Guide JSON"
                } else {
                    "Locale JSON"
                }
                .to_string(),
                requires_reload: false,
                size,
                is_guide,
            });
        }

        files.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Json(DataFilesListResponse { files }))
    })
    .await
}

/// Get a data file (admin)
///
/// The raw text of an editable file: a translation or guide file (`<page>.<locale>.json`),
/// `languages.json` (`{}` when it does not exist) or a community data file. Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/data-files/{filename}",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("filename" = String, Path, description = "File name, e.g. `skills.yaml` or `home.en.json`.")),
    responses(
        (status = 200, description = "The file contents.", body = String, content_type = "text/plain"),
        (
            status = 400,
            description = "`Invalid filename` (contains `..`, `/` or `\\`); `File not editable` \
                           for any other name; `Locale file not found: <page>.<locale>.json`; \
                           `Failed to read file: …` / `Failed to read locale file: …`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn get_data_file(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(filename): Path<String>,
) -> ApiResult<String> {
    account.require_access(ADMIN)?;
    check_filename(&filename)?;
    let content = state.content().clone();

    if let Some((page, locale)) = parse_admin_filename(&filename) {
        return blocking(move || Ok(content.read_locale(&page, &locale)?)).await;
    }
    if filename == LANGUAGE_LABELS_FILENAME {
        return blocking(move || Ok(content.read_language_labels_json()?)).await;
    }
    let Some(file) = editable_data_file(&filename) else {
        return Err(ApiError::bad_request("File not editable"));
    };
    let store = state.data_store()?.clone();
    blocking(move || {
        store
            .read_file(file)
            .map_err(|e| ApiError::bad_request(format!("Failed to read file: {e}")))
    })
    .await
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct SaveQuery {
    /// `guide` saves a `<slug>.<locale>.json` as a guide file (in `guides/<slug>/`, creating the
    /// guide). Ignored for other files.
    kind: Option<String>,
}

/// Save a data file (admin)
///
/// Replaces an editable file with the request body (raw text, any content type):
///
/// - `<page>.<locale>.json`: must be a non-empty JSON object of strings; backed up, then
///   written. Guide pages (and any page with `kind=guide`) go to the guide's directory.
/// - `languages.json`: a JSON object of code → label strings.
/// - A community data file: the whole data set is validated with the new file **before**
///   anything is written (the legacy waitlist wrote first), then the old file is backed up, the
///   new one written atomically and the new data loaded. Warnings (e.g. skill plans that no
///   longer build) are logged, not returned.
///
/// Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/data-files/{filename}",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(
        ("filename" = String, Path, description = "File name, e.g. `skills.yaml` or `home.en.json`."),
        SaveQuery,
    ),
    request_body(content = String, content_type = "text/plain", description = "The new file contents."),
    responses(
        (
            status = 200,
            description = "Saved: `Locale file saved successfully`, `Guide file saved successfully`, \
                           `Language labels saved successfully` or `File saved successfully`.",
            body = String,
            content_type = "text/plain",
            example = "File saved successfully",
        ),
        (
            status = 400,
            description = "`Invalid filename`; `File not editable`; a translation that is not \
                           valid (`Invalid JSON: …`, `Locale file must not be empty`, \
                           `Key \"x\" must have a string value`, …); a community data file \
                           that does not load (the loader's message, e.g. `skills.yaml: invalid \
                           YAML: …` or `data files refer to …`); a failed write.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn save_data_file(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(filename): Path<String>,
    Query(query): Query<SaveQuery>,
    input: String,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    check_filename(&filename)?;
    let content = state.content().clone();

    if let Some((page, locale)) = parse_admin_filename(&filename) {
        let as_guide = query.kind.as_deref() == Some("guide");
        return blocking(move || {
            if as_guide {
                content.save_guide_locale(&page, &locale, &input)?;
                Ok("Guide file saved successfully")
            } else {
                content.save_locale(&page, &locale, &input)?;
                Ok("Locale file saved successfully")
            }
        })
        .await;
    }
    if filename == LANGUAGE_LABELS_FILENAME {
        return blocking(move || {
            content.save_language_labels(&input)?;
            Ok("Language labels saved successfully")
        })
        .await;
    }
    let Some(file) = editable_data_file(&filename) else {
        return Err(ApiError::bad_request("File not editable"));
    };
    let store = state.data_store()?.clone();
    blocking(move || {
        let validation = store.replace_file(file, &input).map_err(data_error)?;
        for warning in validation.warnings {
            tracing::warn!(file = %file, %warning, "data file saved with a warning");
        }
        Ok("File saved successfully")
    })
    .await
}

/// Delete a translation file (admin)
///
/// Deletes a translation or guide file (`<page>.<locale>.json`), keeping a backup. Community
/// data files and `languages.json` cannot be deleted. Requires `commanders-manage:admin`.
#[utoipa::path(
    delete,
    path = "/api/admin/data-files/{filename}",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("filename" = String, Path, description = "Translation file name, e.g. `home.de.json`.")),
    responses(
        (status = 200, description = "Deleted.", body = String, content_type = "text/plain", example = "Locale file deleted successfully"),
        (
            status = 400,
            description = "`Invalid filename`; `Only locale translation files can be deleted from \
                           here`; `Locale file not found: <page>.<locale>.json`; \
                           `Failed to delete locale file: …`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn delete_data_file(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(filename): Path<String>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    check_filename(&filename)?;
    let Some((page, locale)) = parse_admin_filename(&filename) else {
        return Err(ApiError::bad_request(
            "Only locale translation files can be deleted from here",
        ));
    };
    let content = state.content().clone();
    blocking(move || {
        content.delete_locale(&page, &locale)?;
        Ok("Locale file deleted successfully")
    })
    .await
}

/// Reload the data files (admin)
///
/// Reloads the community data from disk, for example after editing a file by hand. Only
/// accepted for the files the legacy waitlist cached (`skills.yaml`, `categories.yaml`,
/// `modules.yaml`, `tags.yaml`, `fits.dat`), but the whole data set is reloaded because the
/// files depend on each other; a failed reload keeps the current data. Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/data-files/{filename}/reload",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("filename" = String, Path, description = "A data file that requires reload, e.g. `fits.dat`.")),
    responses(
        (status = 200, description = "Reloaded.", body = String, content_type = "text/plain", example = "File reloaded successfully"),
        (
            status = 400,
            description = "`Invalid filename`; `File does not require reload` (an editable file \
                           that the legacy waitlist did not cache); `Unknown file type`; \
                           `Failed to reload <name>: …` (e.g. `Failed to reload fits: …`) when the \
                           data set does not load.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn reload_data_file(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(filename): Path<String>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    check_filename(&filename)?;
    let file = match editable_data_file(&filename) {
        Some(file) if file.legacy_requires_reload() => file,
        Some(_) => return Err(ApiError::bad_request("File does not require reload")),
        None => return Err(ApiError::bad_request("Unknown file type")),
    };
    // Data files depend on each other, so the whole data set is reloaded; a failed reload keeps
    // the current data.
    let store = state.data_store()?.clone();
    blocking(move || {
        store.reload().map_err(|e| {
            let what = file.file_name().split('.').next().unwrap_or_default();
            ApiError::bad_request(format!("Failed to reload {what}: {e}"))
        })?;
        Ok("File reloaded successfully")
    })
    .await
}

#[derive(Serialize, ToSchema)]
struct GuideAssetsListResponse {
    /// Sorted by name.
    assets: Vec<GuideAssetInfo>,
}

/// List guide images (admin)
///
/// The images in a guide's directory (backups left out), sorted by name; empty when the guide
/// has no directory. Requires `commanders-manage:admin`.
#[utoipa::path(
    get,
    path = "/api/admin/guides/{slug}/assets",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(("slug" = String, Path, description = "Guide slug.")),
    responses(
        (status = 200, description = "The images.", body = GuideAssetsListResponse),
        (status = 400, description = "An invalid slug (`Page slug must …`); `Failed to read guide directory: …`.", body = String, content_type = "text/plain"),
    ),
)]
async fn list_guide_assets(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path(slug): Path<String>,
) -> ApiResult<Json<GuideAssetsListResponse>> {
    account.require_access(ADMIN)?;
    let content = state.content().clone();
    blocking(move || {
        Ok(Json(GuideAssetsListResponse {
            assets: content.list_assets(&slug)?,
        }))
    })
    .await
}

#[derive(Serialize, ToSchema)]
struct UploadGuideAssetResponse {
    /// The name the image was saved under (an opaque PNG that was optimized becomes `.jpg`).
    filename: String,
    /// Saved size in bytes.
    size: u64,
}

/// Upload a guide image (admin)
///
/// Saves the request body (the raw image bytes, any content type) as an image of the guide,
/// creating the guide directory and backing up an image it replaces. The extension must be
/// `png`, `jpg`, `jpeg`, `gif`, `webp` or `svg`. Images over 1 MiB are resized to at most
/// 1600 px and re-encoded (JPEG at quality 85, an opaque PNG as JPEG under a `.jpg` name, a
/// PNG with transparency recompressed, WebP lossless); SVG and GIF are stored as sent.
/// Requires `commanders-manage:admin`.
#[utoipa::path(
    post,
    path = "/api/admin/guides/{slug}/assets/{filename}",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(
        ("slug" = String, Path, description = "Guide slug."),
        ("filename" = String, Path, description = "Image file name, e.g. `fleet.png`."),
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "The image, at most 10,000,000 bytes."),
    responses(
        (status = 200, description = "Saved.", body = UploadGuideAssetResponse),
        (
            status = 400,
            description = "`Upload exceeds 10 MiB limit` (over 10,000,000 bytes); `Failed to read \
                           upload: …`; `Asset must not be empty`; `Invalid asset filename`; \
                           `Asset must have a file extension`; `Unsupported guide asset type`; \
                           an invalid slug (`Page slug must …`); `Asset still exceeds 10 MiB \
                           after compression; use a smaller image`; a failed write.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn upload_guide_asset(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path((slug, filename)): Path<(String, String)>,
    body: Body,
) -> ApiResult<Json<UploadGuideAssetResponse>> {
    account.require_access(ADMIN)?;
    let bytes = read_upload(body).await?;
    let content = state.content().clone();
    blocking(move || {
        let (filename, size) = content.save_asset(&slug, &filename, &bytes)?;
        Ok(Json(UploadGuideAssetResponse { filename, size }))
    })
    .await
}

/// The raw upload, at most [`MAX_UPLOAD_BYTES`].
async fn read_upload(body: Body) -> ApiResult<Vec<u8>> {
    let mut stream = body.into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ApiError::bad_request(format!("Failed to read upload: {e}")))?;
        if bytes.len() + chunk.len() > MAX_UPLOAD_BYTES {
            return Err(ApiError::bad_request("Upload exceeds 10 MiB limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Delete a guide image (admin)
///
/// Deletes an image of a guide. No backup is kept, as in the legacy waitlist. Requires
/// `commanders-manage:admin`.
#[utoipa::path(
    delete,
    path = "/api/admin/guides/{slug}/assets/{filename}",
    tag = tags::ADMIN,
    security(("session" = ["commanders-manage:admin"])),
    params(
        ("slug" = String, Path, description = "Guide slug."),
        ("filename" = String, Path, description = "Image file name."),
    ),
    responses(
        (status = 200, description = "Deleted.", body = String, content_type = "text/plain", example = "Asset deleted successfully"),
        (
            status = 400,
            description = "`Guide asset not found: <filename>`; `Invalid asset filename`; \
                           `Asset must have a file extension`; `Unsupported guide asset type`; an \
                           invalid slug (`Page slug must …`); `Failed to delete asset: …`.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn delete_guide_asset(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Path((slug, filename)): Path<(String, String)>,
) -> ApiResult<&'static str> {
    account.require_access(ADMIN)?;
    let content = state.content().clone();
    blocking(move || {
        content.delete_asset(&slug, &filename)?;
        Ok("Asset deleted successfully")
    })
    .await
}
