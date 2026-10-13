//! Mapping ESI and SSO failures onto [`EsiError`] with the legacy waitlist's semantics.
//!
//! The legacy backend reported ESI failures differently depending on how a call was made, and
//! the HTTP layer keys off the variant (`MissingScope`/`NoToken` → 401, `WithMessage(code, _)` →
//! `code`, everything else → 500), so each wrapper picks the [`ErrorStyle`] its legacy
//! counterpart had.

use eve_esi_client::ErrorExt as _;
use eve_esi_client::auth::AuthError;
use eve_esi_client::types;

pub use wl_core::EsiError;

/// Fallible ESI call result.
pub type EsiResult<T> = Result<T, EsiError>;

/// How an ESI error response is reported, mirroring the legacy client's per-method behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorStyle {
    /// Authenticated GETs: `WithMessage(status, <ESI's JSON error body>)`, so the API answers
    /// with ESI's status and `ESI error: {"error":"..."}`.
    Body,
    /// POST/PUT (fleet invite, MOTD update, open window): `WithMessage(status, <error text up to
    /// the first ", ">)`, e.g. `ESI error: Character is already in a fleet`.
    Message,
    /// Unauthenticated GETs and DELETEs: `Status(status)`, answered as a 500
    /// `ESI error: ESI returned <status>`.
    Status,
}

/// Convert an eve-esi-client error into an [`EsiError`] in the given style.
pub(crate) fn from_client(
    error: eve_esi_client::Error<types::Error>,
    style: ErrorStyle,
) -> EsiError {
    if let Some(body) = error.esi_error() {
        let status = error.status().map_or(500, |s| s.as_u16());
        return match style {
            ErrorStyle::Body => EsiError::WithMessage(
                status,
                serde_json::to_string(body).unwrap_or_else(|_| body.error.clone()),
            ),
            ErrorStyle::Message => EsiError::WithMessage(status, first_reason(&body.error)),
            ErrorStyle::Status => EsiError::Status(status),
        };
    }
    match &error {
        // A status ESI's spec doesn't list (e.g. a 3xx): no body worth showing.
        eve_esi_client::Error::UnexpectedResponse(response) => {
            EsiError::Status(response.status().as_u16())
        }
        _ => EsiError::Http(error.to_string().into()),
    }
}

/// The legacy client kept only the part of a POST error before the first `", "`: CCP's fleet
/// invite errors pack extra detail after it.
fn first_reason(error: &str) -> String {
    error.split(", ").next().unwrap_or(error).to_string()
}

/// Database failures while loading or storing tokens.
pub(crate) fn db(error: sqlx::Error) -> EsiError {
    EsiError::Database(Box::new(error))
}

/// SSO failures other than a revoked token (which callers turn into [`EsiError::NoToken`]).
pub(crate) fn sso(error: AuthError) -> EsiError {
    EsiError::Http(Box::new(error))
}

/// Questions callers ask about an [`EsiError`].
pub trait EsiErrorExt {
    /// The HTTP status ESI answered with, for `Status` and `WithMessage`.
    fn esi_status(&self) -> Option<u16>;

    /// Whether ESI answered 404 (e.g. "not in a fleet", "not the fleet boss", a deleted war).
    fn is_not_found(&self) -> bool {
        self.esi_status() == Some(404)
    }

    /// Whether the character can't be acted for: no stored token, the token lacks the scope, or
    /// ESI refused (403) or didn't find (404) what was asked. The fleet updater drops a fleet
    /// whose boss hits one of these.
    fn is_unusable_character(&self) -> bool;
}

impl EsiErrorExt for EsiError {
    fn esi_status(&self) -> Option<u16> {
        match self {
            EsiError::Status(code) | EsiError::WithMessage(code, _) => Some(*code),
            _ => None,
        }
    }

    fn is_unusable_character(&self) -> bool {
        matches!(
            self,
            EsiError::NoToken
                | EsiError::MissingScope
                | EsiError::WithMessage(403 | 404, _)
                | EsiError::Status(403 | 404)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_reason_keeps_text_before_the_first_separator() {
        assert_eq!(
            first_reason("Character is already in a fleet, character_id: 1"),
            "Character is already in a fleet"
        );
        assert_eq!(first_reason("plain"), "plain");
    }

    #[test]
    fn display_strings_match_the_legacy_wire_format() {
        assert_eq!(EsiError::MissingScope.to_string(), "missing ESI scope");
        assert_eq!(EsiError::NoToken.to_string(), "no ESI token found");
        assert_eq!(EsiError::Status(502).to_string(), "ESI returned 502");
        assert_eq!(
            EsiError::WithMessage(400, "nope".into()).to_string(),
            "nope"
        );
    }

    #[test]
    fn status_helpers() {
        assert!(EsiError::WithMessage(404, String::new()).is_not_found());
        assert!(EsiError::Status(404).is_not_found());
        assert!(!EsiError::NoToken.is_not_found());
        assert!(EsiError::NoToken.is_unusable_character());
        assert!(EsiError::WithMessage(403, String::new()).is_unusable_character());
        assert!(!EsiError::WithMessage(500, String::new()).is_unusable_character());
        assert!(!EsiError::Http("x".into()).is_unusable_character());
    }
}
