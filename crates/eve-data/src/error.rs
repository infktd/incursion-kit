//! Error types.

use std::path::PathBuf;

/// Loading the static data file failed.
#[derive(Debug, thiserror::Error)]
pub enum EveDataError {
    #[error("static data file {} does not exist; run `waitlist sde` to build it", .0.display())]
    Missing(PathBuf),
    #[error("could not read static data file {}: {source}", .path.display())]
    Database {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("static data file {} contains no types", .0.display())]
    Empty(PathBuf),
}

/// A type lookup failed. The message matches the legacy waitlist, which API error bodies expose
/// (`type error: no matches for item`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TypeError {
    #[error("no matches for item")]
    NothingMatched,
}
