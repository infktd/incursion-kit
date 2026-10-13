//! The error type of the shared services and workers.

use wl_core::EsiError;

/// A failed ESI call or database query. The API maps `Esi` exactly like a direct ESI failure and
/// `Database` to `500 database error`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ESI error: {0}")]
    Esi(#[from] EsiError),
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
