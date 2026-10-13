//! ESI failure kinds shared between the ESI client and the HTTP error mapping.
//!
//! The API maps each variant to a fixed status code and plain-text body (see `wl-api`'s
//! `ApiError`), mirroring the legacy waitlist: `MissingScope`/`NoToken` → 401,
//! `WithMessage(code, _)` → `code`, everything else → 500. The `Display` strings are part of the
//! wire contract (the body is `ESI error: <display>`), so keep them unchanged.

/// Boxed error source, used where the underlying error type belongs to another crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
pub enum EsiError {
    /// Storing or loading tokens failed.
    #[error("database error")]
    Database(#[source] BoxError),
    /// Transport-level failure talking to ESI or the SSO.
    #[error("ESI http error")]
    Http(#[source] BoxError),
    /// ESI answered with an unexpected status.
    #[error("ESI returned {0}")]
    Status(u16),
    /// ESI rejected a request with a message that should reach the user (e.g. a fleet invite
    /// failure). The API answers with this status code and `ESI error: <message>`.
    #[error("{1}")]
    WithMessage(u16, String),
    /// No (valid) refresh token stored for the character.
    #[error("no ESI token found")]
    NoToken,
    /// The stored token lacks the required scope.
    #[error("missing ESI scope")]
    MissingScope,
}
