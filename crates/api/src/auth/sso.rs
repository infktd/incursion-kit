//! CSRF protection for EVE SSO logins.
//!
//! The legacy waitlist sent a fixed OAuth `state` (`normal`, `alt`, `srp_admin`, `srp_setup`), so
//! anyone could make a victim's browser complete *their* login (login CSRF), or link an attacker's
//! character as the victim's alt. Every login URL now carries `state=<kind>:<nonce>` and sets the
//! nonce in a short-lived, `HttpOnly` cookie scoped to `/api/auth`; the callback only accepts a
//! state whose nonce matches the cookie, then clears it.
//!
//! The frontend contract is unchanged except that the callback page must compare the state's
//! prefix (`state.split(":")[0]`) instead of the whole value.
//!
//! Any endpoint that hands out a login URL (e.g. the SRP service-account setup) must use
//! [`start_login`] and send the returned [`SsoNonceCookie`] with its response.

use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponseParts, ResponseParts};
use axum_extra::extract::CookieJar;
use wl_esi::LoginKind;

use crate::{ApiError, ApiResult, AppState};

/// Name of the cookie holding the pending login's nonce.
pub const NONCE_COOKIE: &str = "wlSsoNonce";
/// How long a login may take, in seconds.
pub const NONCE_MAX_AGE: u64 = 900;
/// Error body when the callback's state doesn't match this browser's pending login.
pub const STATE_MISMATCH: &str = "Login state mismatch: the login expired or was started in another browser. Please log in again.";

/// A `Set-Cookie` for the nonce cookie, usable as a response part:
/// `(cookie, Json(body))` or `(cookie, text)`.
#[derive(Debug, Clone)]
pub struct SsoNonceCookie(HeaderValue);

impl SsoNonceCookie {
    fn new(value: &str, max_age: u64, secure: bool) -> ApiResult<Self> {
        let mut cookie = format!(
            "{NONCE_COOKIE}={value}; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age={max_age}"
        );
        if secure {
            cookie.push_str("; Secure");
        }
        HeaderValue::try_from(cookie)
            .map(Self)
            .map_err(ApiError::internal)
    }

    /// Clears the nonce once a login completed.
    pub fn cleared(secure: bool) -> ApiResult<Self> {
        Self::new("", 0, secure)
    }
}

impl IntoResponseParts for SsoNonceCookie {
    type Error = std::convert::Infallible;

    fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
        res.headers_mut().append(header::SET_COOKIE, self.0);
        Ok(res)
    }
}

/// The EVE SSO URL for a `kind` login (`fc` adds the fleet-command scopes) and the cookie that
/// binds its callback to this browser.
pub fn start_login(
    state: &AppState,
    kind: LoginKind,
    fc: bool,
) -> ApiResult<(String, SsoNonceCookie)> {
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    let url = state
        .esi()?
        .login_url_with_state(&kind.scopes(fc), &format!("{}:{nonce}", kind.state()));
    Ok((
        url,
        SsoNonceCookie::new(&nonce, NONCE_MAX_AGE, state.secure_cookies())?,
    ))
}

/// Checks a callback's `state` against the browser's nonce cookie and returns the login kind
/// (`None` for an unknown kind, which the legacy callback treated as a normal login).
///
/// Fails with 400 [`STATE_MISMATCH`] when the state has no nonce, there is no cookie, or they
/// differ.
pub fn verify_state(headers: &HeaderMap, state: &str) -> ApiResult<Option<LoginKind>> {
    let jar = CookieJar::from_headers(headers);
    let expected = jar.get(NONCE_COOKIE).map(|c| c.value()).unwrap_or("");
    let given = state.split_once(':').map(|(_, nonce)| nonce).unwrap_or("");
    if expected.is_empty() || !constant_time_eq(expected.as_bytes(), given.as_bytes()) {
        return Err(ApiError::bad_request(STATE_MISMATCH));
    }
    Ok(LoginKind::from_state(state))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookie: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        headers
    }

    #[test]
    fn accepts_only_a_matching_nonce() {
        let h = headers("authToken=x; wlSsoNonce=abc123");
        assert_eq!(
            verify_state(&h, "alt:abc123").unwrap(),
            Some(LoginKind::Alt)
        );
        assert_eq!(verify_state(&h, "weird:abc123").unwrap(), None);
        for bad in ["alt", "alt:", "alt:abc124", "alt:abc1234", ""] {
            assert!(verify_state(&h, bad).is_err(), "{bad}");
        }
        assert!(verify_state(&HeaderMap::new(), "alt:abc123").is_err());
        assert!(verify_state(&headers("wlSsoNonce="), "alt:").is_err());
    }

    #[test]
    fn cookie_attributes() {
        let cookie = SsoNonceCookie::new("n", 900, true).unwrap();
        assert_eq!(
            cookie.0,
            "wlSsoNonce=n; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=900; Secure"
        );
        let cleared = SsoNonceCookie::cleared(false).unwrap();
        assert_eq!(
            cleared.0,
            "wlSsoNonce=; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=0"
        );
    }
}
