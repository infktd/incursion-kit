//! Building the static data file from CCP's static data export (SDE).
//!
//! CCP publishes the SDE as a zip of JSON-lines files at
//! `https://developers.eveonline.com/static-data/tranquility/`. [`build`] downloads one build
//! (the latest unless pinned), keeps only what the waitlist needs (types, group categories,
//! variations, a handful of dogma attributes and the slot effects, plus the star map for the
//! incursion tracker: regions, constellations, systems with their size, stargates and NPC
//! stations) and writes it to a small SQLite file. The file is written next to its destination and renamed into place, so readers
//! never see a half-written file. The SDE build number is recorded in the file (`sdeInfo` table)
//! and logged at startup.
//!
//! The SQLite layout keeps the table and column names of the legacy waitlist's
//! `sqlite-shrunk.sqlite`, so an existing legacy file can be loaded as well.

mod download;
mod extract;
mod map;
mod sqlite;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) use sqlite::read_tables;

/// Where CCP publishes the SDE for the live server.
pub const DEFAULT_BASE_URL: &str = "https://developers.eveonline.com/static-data/tranquility";

/// Which SDE build a static data file was made from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdeInfo {
    pub build_number: u64,
    /// Release date as published by CCP (RFC 3339), when known.
    pub release_date: Option<String>,
}

/// How to build the static data file.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// SDE build to download; the latest build when `None`.
    pub build_number: Option<u64>,
    /// Base URL of the SDE distribution (see [`DEFAULT_BASE_URL`]).
    pub base_url: String,
    /// Convert this already-downloaded SDE zip (`*-jsonl.zip`) instead of downloading one.
    pub archive: Option<PathBuf>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            build_number: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            archive: None,
        }
    }
}

/// What [`ensure_sde`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// The file already existed and was left alone.
    Present,
    /// The file was missing and has been built.
    Built(SdeInfo),
    /// The file had an older layout (e.g. no star map) and has been rebuilt.
    Upgraded(SdeInfo),
}

/// Building the static data file failed.
#[derive(Debug, thiserror::Error)]
pub enum SdeError {
    #[error("could not download the SDE: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected SDE version response: {0}")]
    BadVersion(String),
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read the SDE archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("the SDE archive has no {0}")]
    MissingFile(&'static str),
    #[error("{file} line {line}: {message}")]
    Invalid {
        file: &'static str,
        line: usize,
        message: String,
    },
    #[error("could not write the static data file: {0}")]
    Database(#[from] sqlx::Error),
    #[error("SDE conversion task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> SdeError + '_ {
    move |source| SdeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Builds the static data file at `out`, replacing it atomically if it exists.
pub async fn build(out: &Path, options: &BuildOptions) -> Result<SdeInfo, SdeError> {
    let parent = match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(io_error(&parent))?;

    let (archive, requested) = match &options.archive {
        Some(archive) => (TempFile::keep(archive.clone()), options.build_number),
        None => {
            let client = reqwest::Client::builder()
                .user_agent(concat!("incursion-waitlist/", env!("CARGO_PKG_VERSION")))
                .build()?;
            let build_number = match options.build_number {
                Some(build_number) => build_number,
                None => {
                    download::latest(&client, &options.base_url)
                        .await?
                        .build_number
                }
            };
            let path = parent.join(format!(".sde-{build_number}.zip.partial"));
            let archive = TempFile::remove_on_drop(path);
            download::archive(&client, &options.base_url, build_number, archive.path()).await?;
            (archive, Some(build_number))
        }
    };

    let archive_path = archive.path().to_path_buf();
    let extracted = tokio::task::spawn_blocking(move || extract::extract(&archive_path)).await??;
    drop(archive);

    let info = match (extracted.info.clone(), requested) {
        (Some(info), _) => info,
        (None, Some(build_number)) => SdeInfo {
            build_number,
            release_date: None,
        },
        (None, None) => {
            return Err(SdeError::MissingFile(extract::INFO_FILE));
        }
    };
    tracing::info!(
        build = info.build_number,
        types = extracted.types.len(),
        "converted SDE"
    );

    let partial = TempFile::remove_on_drop(out.with_extension("sqlite.partial"));
    sqlite::write(partial.path(), &extracted, &info).await?;
    std::fs::rename(partial.path(), out).map_err(io_error(out))?;
    partial.disarm();

    tracing::info!(path = %out.display(), build = info.build_number, "static data file written");
    Ok(info)
}

/// Builds the static data file at `path` unless it already exists.
///
/// A file in an older layout (schema version below the current one, e.g. without the star map)
/// is rebuilt; if that fails the old file is kept and the failure is only logged, so a server
/// without network access still starts.
pub async fn ensure_sde(path: &Path, options: &BuildOptions) -> Result<EnsureOutcome, SdeError> {
    if path.exists() {
        let version = sqlite::schema_version(path).await.unwrap_or(0);
        if version >= sqlite::CURRENT_SCHEMA_VERSION {
            return Ok(EnsureOutcome::Present);
        }
        tracing::info!(
            path = %path.display(),
            version,
            "static data file has an older layout, rebuilding it from the SDE"
        );
        return match build(path, options).await {
            Ok(info) => Ok(EnsureOutcome::Upgraded(info)),
            Err(e) => {
                tracing::warn!(error = %e, "could not rebuild the static data file; keeping the old one");
                Ok(EnsureOutcome::Present)
            }
        };
    }
    tracing::info!(path = %path.display(), "static data file missing, building it from the SDE");
    build(path, options).await.map(EnsureOutcome::Built)
}

/// Asks CCP which SDE build is the latest.
pub async fn latest_build(base_url: &str) -> Result<SdeInfo, SdeError> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("incursion-waitlist/", env!("CARGO_PKG_VERSION")))
        .build()?;
    download::latest(&client, base_url).await
}

/// A file that is deleted when dropped unless disarmed (or created with [`TempFile::keep`]).
struct TempFile {
    path: PathBuf,
    remove: bool,
}

impl TempFile {
    fn remove_on_drop(path: PathBuf) -> Self {
        Self { path, remove: true }
    }

    fn keep(path: PathBuf) -> Self {
        Self {
            path,
            remove: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(mut self) {
        self.remove = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.remove {
            // Best effort: the file may never have been created.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests;
