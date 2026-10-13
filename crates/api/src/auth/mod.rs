//! Authentication (session cookie → account) and authorization (permission keys, characters).
//!
//! - [`AuthenticatedAccount`] is an axum extractor. Use it as a handler argument to require a
//!   logged-in account (401 `401 Authorization Required` otherwise), or `Option<AuthenticatedAccount>`
//!   for endpoints where login is optional.
//! - [`AuthenticatedAccount::require_access`] checks a permission key (401 `access denied`).
//! - [`AuthenticatedAccount::authorize_character`] checks that the account may act for a
//!   character: its main, one of its alts, or anyone if it holds the override permission.

pub mod permissions;
pub mod session;
pub mod sso;

use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum_extra::extract::CookieJar;

use crate::{ApiError, AppState};
use permissions::AccessSet;
pub use session::{Session, SessionKey, SetSessionCookie};

/// The logged-in account.
#[derive(Debug, Clone)]
pub struct AuthenticatedAccount {
    /// Account id = the main character's id.
    pub id: i64,
    /// Permission keys granted by the account's role.
    pub access: &'static AccessSet,
    /// Character used for in-game UI calls when different from the main (SRP admin re-auth).
    pub window_character_id: Option<i64>,
}

impl AuthenticatedAccount {
    pub fn has_access(&self, key: &str) -> bool {
        self.access.contains(key)
    }

    /// `Err(AccessDenied)` (401 `access denied`) unless the account has `key`.
    pub fn require_access(&self, key: &str) -> Result<(), ApiError> {
        if self.has_access(key) {
            Ok(())
        } else {
            Err(ApiError::AccessDenied)
        }
    }

    /// Ok if `character_id` is the account's main, the account holds `permission_override`, or
    /// `character_id` is one of the account's alts. Otherwise `AccessDenied`.
    pub async fn authorize_character(
        &self,
        db: &wl_db::Db,
        character_id: i64,
        permission_override: Option<&str>,
    ) -> Result<(), ApiError> {
        if self.id == character_id {
            return Ok(());
        }
        if permission_override.is_some_and(|p| self.has_access(p)) {
            return Ok(());
        }
        let alt: Option<i64> = sqlx::query_scalar(
            "SELECT alt_id FROM alt_character WHERE account_id = ? AND alt_id = ?",
        )
        .bind(self.id)
        .bind(character_id)
        .fetch_optional(db)
        .await?;
        match alt {
            Some(_) => Ok(()),
            None => Err(ApiError::AccessDenied),
        }
    }

    /// Resolve a session to an account: look up the role of the main character.
    ///
    /// `Ok(None)` if the role is unknown (the legacy waitlist treats that as an invalid token).
    pub async fn from_session(db: &wl_db::Db, session: Session) -> Result<Option<Self>, ApiError> {
        let role: Option<String> =
            sqlx::query_scalar("SELECT role FROM admin WHERE character_id = ?")
                .bind(session.account_id)
                .fetch_optional(db)
                .await?;
        let role = role.as_deref().unwrap_or("user");
        Ok(permissions::access_keys(role).map(|access| Self {
            id: session.account_id,
            access,
            window_character_id: session.window_character_id,
        }))
    }
}

/// Decode the session cookie, if present and valid.
fn session_from_parts(parts: &Parts, state: &AppState) -> Option<Session> {
    let jar = CookieJar::from_headers(&parts.headers);
    let cookie = jar.get(session::COOKIE_NAME)?;
    state.session_key().decode(cookie.value()).ok()
}

impl FromRequestParts<AppState> for AuthenticatedAccount {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        <Self as OptionalFromRequestParts<AppState>>::from_request_parts(parts, state)
            .await?
            .ok_or(ApiError::Unauthenticated)
    }
}

/// `Option<AuthenticatedAccount>`: `None` when there is no valid session; database errors still
/// fail the request (500).
impl OptionalFromRequestParts<AppState> for AuthenticatedAccount {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, ApiError> {
        match session_from_parts(parts, state) {
            Some(session) => Self::from_session(state.db(), session).await,
            None => Ok(None),
        }
    }
}
