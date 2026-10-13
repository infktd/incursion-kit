//! SRP errors. The API maps them onto its plain-text error responses (`BadRequest` → 400 with
//! the message, `NotFound` → 404, `Forbidden` → 403, `Database` → 500 `database error`, `Esi`
//! like any other ESI error).

use wl_core::EsiError;

pub type SrpResult<T> = Result<T, SrpError>;

#[derive(Debug, thiserror::Error)]
pub enum SrpError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ESI error: {0}")]
    Esi(#[from] EsiError),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Forbidden(String),
}

impl SrpError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub(crate) fn report_not_found() -> Self {
        Self::NotFound("SRP report not found".into())
    }
}
