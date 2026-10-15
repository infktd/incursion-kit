//! The API error type and its HTTP mapping.
//!
//! Mirrors the legacy waitlist's error responder exactly: every error is a **plain-text** body
//! with a fixed status code. Notably, a missing permission is `401 access denied` (not 403), and
//! a missing ESI token or scope is also 401. Clients tell those apart by the body text, so the
//! `Display` strings below are part of the wire contract.
//!
//! | variant | status | body |
//! |---|---|---|
//! | [`ApiError::Unauthenticated`] | 401 | `401 Authorization Required` |
//! | [`ApiError::AccessDenied`] | 401 | `access denied` |
//! | `Esi(MissingScope)` / `Esi(NoToken)` | 401 | `ESI error: missing ESI scope` / `ESI error: no ESI token found` |
//! | `Esi(WithMessage(code, msg))` | `code` | `ESI error: msg` |
//! | `Esi(_)` (other) | 500 | `ESI error: …` |
//! | [`ApiError::BadRequest`], `Type`, `Fit`, `SkillPlan` | 400 | message / `type error: …` / `fit error: …` / `skill plan error: …` |
//! | [`ApiError::Forbidden`] | 403 | message |
//! | [`ApiError::NotFound`] | 404 | message |
//! | [`ApiError::UnprocessableEntity`] | 422 | message (malformed JSON body) |
//! | [`ApiError::Invalid`] | 400 | JSON [`ValidationReport`] (structured data editors only) |
//! | [`ApiError::Database`] | 500 | `database error` |
//! | [`ApiError::Internal`] | 500 | `internal error` |

use axum::Json;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;
use wl_core::EsiError;
use wl_core::esi_error::BoxError;
use wl_fitting::editor::FieldError;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("ESI error: {0}")]
    Esi(#[from] EsiError),
    #[error("type error: {0}")]
    Type(#[source] BoxError),
    #[error("fit error: {0}")]
    Fit(#[source] BoxError),
    #[error("skill plan error: {0}")]
    SkillPlan(#[source] BoxError),

    #[error("{0}")]
    BadRequest(String),
    /// No valid session cookie (or an unknown role on the account).
    #[error("401 Authorization Required")]
    Unauthenticated,
    /// The account lacks a permission, or may not act for the requested character.
    #[error("access denied")]
    AccessDenied,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    /// A request body that is not valid JSON for the endpoint.
    #[error("{0}")]
    UnprocessableEntity(String),
    /// A structured data edit that is not valid: `400` with a JSON [`ValidationReport`] body
    /// (the only error that is not plain text; the data editor endpoints use it).
    #[error("invalid data: {}", .0.summary())]
    Invalid(Box<ValidationReport>),
    /// Anything else that should never happen (serialization bugs, missing services...). The
    /// details are logged, not sent.
    #[error("internal error")]
    Internal(#[source] BoxError),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn internal(err: impl Into<BoxError>) -> Self {
        Self::Internal(err.into())
    }

    /// A structured edit with these errors.
    pub fn invalid(dry_run: bool, errors: Vec<FieldError>, warnings: Vec<FieldError>) -> Self {
        Self::Invalid(Box::new(ValidationReport {
            valid: false,
            dry_run,
            errors,
            warnings: warnings.iter().map(FieldError::text).collect(),
            warning_details: warnings,
        }))
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthenticated
            | Self::AccessDenied
            | Self::Esi(EsiError::MissingScope | EsiError::NoToken) => StatusCode::UNAUTHORIZED,
            Self::Esi(EsiError::WithMessage(code, _)) => {
                StatusCode::from_u16(*code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            }
            Self::Database(_)
            | Self::Internal(_)
            | Self::Esi(EsiError::Database(_) | EsiError::Http(_) | EsiError::Status(_)) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::UnprocessableEntity(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BadRequest(_)
            | Self::Type(_)
            | Self::Fit(_)
            | Self::SkillPlan(_)
            | Self::Invalid(_) => StatusCode::BAD_REQUEST,
        }
    }
}

impl From<crate::auth::session::SessionError> for ApiError {
    fn from(err: crate::auth::session::SessionError) -> Self {
        Self::internal(err)
    }
}

/// Content (locale/guide file) errors were all `400 <message>` in the legacy waitlist.
impl From<wl_content::ContentError> for ApiError {
    fn from(err: wl_content::ContentError) -> Self {
        Self::BadRequest(err.0)
    }
}

/// An unknown type id (e.g. a hull in history rows): `400 type error: no matches for item`.
impl From<wl_eve_data::TypeError> for ApiError {
    fn from(err: wl_eve_data::TypeError) -> Self {
        Self::Type(Box::new(err))
    }
}

/// Failures of the shared worker services (affiliation, war cache, skills): an ESI failure maps
/// like a direct ESI call, a database failure to `500 database error`.
impl From<wl_workers::Error> for ApiError {
    fn from(err: wl_workers::Error) -> Self {
        match err {
            wl_workers::Error::Esi(e) => Self::Esi(e),
            wl_workers::Error::Database(e) => Self::Database(e),
        }
    }
}

/// The outcome of a structured data edit or its dry run (`?dry_run=true`): the body of a
/// successful write, and of the `400` of an invalid one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ValidationReport {
    /// Whether the request is valid (and, unless a dry run, was saved).
    pub valid: bool,
    /// Whether this was a dry run: nothing was written.
    pub dry_run: bool,
    /// What is wrong, field by field (empty when valid).
    pub errors: Vec<FieldError>,
    /// Problems that do not prevent saving, e.g. skill plans that no longer build or rules that
    /// name a category that does not exist, as text (`line N: …` or `path: …` when they have
    /// one). Kept for older clients; `warning_details` has the same warnings structured.
    pub warnings: Vec<String>,
    /// The same warnings, in the same order, like `errors`: the field `path` (empty for the
    /// data set as a whole), the `message` without the prefix, and the `line` in EFT text.
    pub warning_details: Vec<FieldError>,
}

impl ValidationReport {
    /// A valid request.
    pub fn valid(dry_run: bool, warnings: Vec<FieldError>) -> Self {
        Self {
            valid: true,
            dry_run,
            errors: Vec::new(),
            warnings: warnings.iter().map(FieldError::text).collect(),
            warning_details: warnings,
        }
    }

    fn summary(&self) -> String {
        self.errors
            .iter()
            .map(|error| {
                if error.path.is_empty() {
                    error.message.clone()
                } else {
                    format!("{}: {}", error.path, error.message)
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Invalid(report) = self {
            return (StatusCode::BAD_REQUEST, Json(*report)).into_response();
        }
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = %self, details = ?self, "request failed");
        }
        (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            self.to_string(),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(err: ApiError, status: u16, body: &str) {
        assert_eq!(err.status().as_u16(), status, "{err:?}");
        assert_eq!(err.to_string(), body);
    }

    #[test]
    fn matches_legacy_mapping() {
        check(ApiError::AccessDenied, 401, "access denied");
        check(ApiError::Unauthenticated, 401, "401 Authorization Required");
        check(
            EsiError::MissingScope.into(),
            401,
            "ESI error: missing ESI scope",
        );
        check(
            EsiError::NoToken.into(),
            401,
            "ESI error: no ESI token found",
        );
        check(
            EsiError::WithMessage(420, "Fleet invite failed".into()).into(),
            420,
            "ESI error: Fleet invite failed",
        );
        check(
            EsiError::Status(502).into(),
            500,
            "ESI error: ESI returned 502",
        );
        check(
            EsiError::Http("boom".into()).into(),
            500,
            "ESI error: ESI http error",
        );
        check(sqlx::Error::RowNotFound.into(), 500, "database error");
        check(ApiError::bad_request("nope"), 400, "nope");
        check(ApiError::Fit("bad".into()), 400, "fit error: bad");
        check(ApiError::Type("bad".into()), 400, "type error: bad");
        check(
            ApiError::SkillPlan("bad".into()),
            400,
            "skill plan error: bad",
        );
        check(ApiError::forbidden("{\"x\":1}"), 403, "{\"x\":1}");
        check(ApiError::not_found(""), 404, "");
        check(ApiError::internal("secret detail"), 500, "internal error");
    }

    #[tokio::test]
    async fn responds_with_plain_text() {
        let response = ApiError::AccessDenied.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"access denied");
    }
}
