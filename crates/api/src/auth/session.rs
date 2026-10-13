//! The session cookie: `authToken`, a Branca token over a msgpack payload.
//!
//! Byte-compatible with the legacy waitlist, so sessions survive the migration as long as
//! `app.token_secret` is kept: Branca (XChaCha20-Poly1305) keyed by the 32-byte secret, payload
//! `rmp_serde::to_vec_named({version: 1, account_id, window_character_id})`, TTL 31 days checked
//! on decode. There is no sliding refresh; a cookie is only (re)issued on login.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// Cookie name.
pub const COOKIE_NAME: &str = "authToken";
/// Token lifetime checked on decode (seconds).
pub const TOKEN_TTL_SECS: u64 = 31 * 86400;
/// `Max-Age` of the cookie (seconds).
pub const COOKIE_MAX_AGE: u64 = 2_678_400;

const TOKEN_VERSION: i32 = 1;

/// The decoded contents of a session token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    /// The account, i.e. the main character id.
    pub account_id: i64,
    /// Character whose ESI token is used for in-game UI calls (set by the SRP admin re-auth).
    pub window_character_id: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct TokenPayload {
    version: i32,
    account_id: i64,
    window_character_id: Option<i64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("invalid session token")]
    InvalidToken,
    #[error("could not encode session token")]
    Encode,
}

/// The Branca key (`app.token_secret`). Never printed.
#[derive(Clone)]
pub struct SessionKey([u8; 32]);

impl fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

impl SessionKey {
    pub fn new(key: [u8; 32]) -> Self {
        Self(key)
    }

    /// Create a token for `session`, timestamped now.
    pub fn encode(&self, session: Session) -> Result<String, SessionError> {
        let payload = rmp_serde::to_vec_named(&TokenPayload {
            version: TOKEN_VERSION,
            account_id: session.account_id,
            window_character_id: session.window_character_id,
        })
        .map_err(|_| SessionError::Encode)?;
        // Branca timestamps are u32 seconds; valid until 2106.
        let now = u32::try_from(unix_now()).map_err(|_| SessionError::Encode)?;
        branca::encode(&payload, &self.0, now).map_err(|_| SessionError::Encode)
    }

    /// Decode and validate a token against the current time.
    pub fn decode(&self, token: &str) -> Result<Session, SessionError> {
        self.decode_at(token, unix_now())
    }

    /// Decode and validate a token as if the current time were `now` (unix seconds).
    pub fn decode_at(&self, token: &str, now: u64) -> Result<Session, SessionError> {
        let (issued_at, payload) = branca::decode_with_timestamp(token, &self.0)
            .map_err(|_| SessionError::InvalidToken)?;
        if u64::from(issued_at) + TOKEN_TTL_SECS < now {
            return Err(SessionError::InvalidToken);
        }
        let decoded: TokenPayload =
            rmp_serde::from_slice(&payload).map_err(|_| SessionError::InvalidToken)?;
        if decoded.version != TOKEN_VERSION || decoded.account_id <= 0 {
            return Err(SessionError::InvalidToken);
        }
        Ok(Session {
            account_id: decoded.account_id,
            window_character_id: decoded.window_character_id,
        })
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The exact `Set-Cookie` value the legacy waitlist sends.
pub fn cookie_header(token: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={COOKIE_MAX_AGE}"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// A `200` response with an empty body that sets (or, with an empty token, clears) the session
/// cookie.
#[derive(Debug, Clone)]
pub struct SetSessionCookie {
    pub token: String,
    pub secure: bool,
}

impl SetSessionCookie {
    /// Overwrites the cookie with an empty value (logout), exactly like the legacy waitlist.
    pub fn cleared(secure: bool) -> Self {
        Self {
            token: String::new(),
            secure,
        }
    }
}

impl IntoResponse for SetSessionCookie {
    fn into_response(self) -> Response {
        match HeaderValue::try_from(cookie_header(&self.token, self.secure)) {
            Ok(value) => (StatusCode::OK, [(header::SET_COOKIE, value)]).into_response(),
            Err(e) => crate::ApiError::internal(e).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    /// Issued at this unix time by the fixture generator below.
    const FIXTURE_ISSUED_AT: u64 = 1_760_000_000;

    // Produced with the legacy waitlist's exact dependency versions (branca 0.10.0,
    // rmp-serde 0.15.5 with rmp 0.8.10) and its token code:
    //   rmp_serde::to_vec_named(&AuthToken { version: 1, account_id, window_character_id })
    //   Branca::new(key).set_timestamp(1_760_000_000).encode(&payload)
    const LEGACY_PAYLOAD_NO_WINDOW: &str = "83a776657273696f6e01aa6163636f756e745f6964ce7dec1b14b377696e646f775f6368617261637465725f6964c0";
    const LEGACY_TOKEN_NO_WINDOW: &str = "92QVEenBEGfKe0uNnhZNnRePNGdztykkMDMo2fJDrrrmQIfL4aCk6rPG88mKUvwtKKjs7cVx6UCIExU0hLKbrTataddzSWWNoxinutVfVIMYZRpZ47Vl5XzydcW5";
    const LEGACY_PAYLOAD_WINDOW: &str = "83a776657273696f6e01aa6163636f756e745f6964ce7dec1b14b377696e646f775f6368617261637465725f6964ce05b0b01b";
    const LEGACY_TOKEN_WINDOW: &str = "gNMmygNLeFRFDDJFSQRl26pOop5Y28NIlVWJfpQa0Z4n3piIPjZP8vcqtYsBJUELQm5hF70NQidbdxxLm6v3pxKPJYRsmlXudOQSYm9Uhvfc38mm8LRxqBShvUoJqyNXI";

    fn key() -> SessionKey {
        let bytes = hex::decode(KEY_HEX).unwrap();
        SessionKey::new(bytes.try_into().unwrap())
    }

    #[test]
    fn decodes_legacy_tokens() {
        let at = FIXTURE_ISSUED_AT + 10;
        assert_eq!(
            key().decode_at(LEGACY_TOKEN_NO_WINDOW, at).unwrap(),
            Session {
                account_id: 2_112_625_428,
                window_character_id: None
            }
        );
        assert_eq!(
            key().decode_at(LEGACY_TOKEN_WINDOW, at).unwrap(),
            Session {
                account_id: 2_112_625_428,
                window_character_id: Some(95_465_499)
            }
        );
    }

    #[test]
    fn payload_bytes_match_legacy_encoding() {
        for (window, expected) in [
            (None, LEGACY_PAYLOAD_NO_WINDOW),
            (Some(95_465_499), LEGACY_PAYLOAD_WINDOW),
        ] {
            let payload = rmp_serde::to_vec_named(&TokenPayload {
                version: 1,
                account_id: 2_112_625_428,
                window_character_id: window,
            })
            .unwrap();
            assert_eq!(hex::encode(payload), expected);
        }
    }

    #[test]
    fn enforces_ttl() {
        let k = key();
        let last_valid = FIXTURE_ISSUED_AT + TOKEN_TTL_SECS;
        assert!(k.decode_at(LEGACY_TOKEN_NO_WINDOW, last_valid).is_ok());
        assert_eq!(
            k.decode_at(LEGACY_TOKEN_NO_WINDOW, last_valid + 1),
            Err(SessionError::InvalidToken)
        );
    }

    #[test]
    fn round_trips_and_rejects_garbage() {
        let k = key();
        let session = Session {
            account_id: 42,
            window_character_id: Some(7),
        };
        let token = k.encode(session).unwrap();
        assert_eq!(k.decode(&token).unwrap(), session);

        assert_eq!(k.decode(""), Err(SessionError::InvalidToken));
        assert_eq!(k.decode("not a token"), Err(SessionError::InvalidToken));
        let other = SessionKey::new([9; 32]);
        assert_eq!(other.decode(&token), Err(SessionError::InvalidToken));
    }

    #[test]
    fn rejects_bad_payloads() {
        let k = key();
        let encode = |payload: &TokenPayload| {
            let bytes = rmp_serde::to_vec_named(payload).unwrap();
            branca::encode(&bytes, &k.0, unix_now() as u32).unwrap()
        };
        let wrong_version = encode(&TokenPayload {
            version: 2,
            account_id: 1,
            window_character_id: None,
        });
        let bad_account = encode(&TokenPayload {
            version: 1,
            account_id: 0,
            window_character_id: None,
        });
        assert_eq!(k.decode(&wrong_version), Err(SessionError::InvalidToken));
        assert_eq!(k.decode(&bad_account), Err(SessionError::InvalidToken));
    }

    #[test]
    fn cookie_attributes_match_legacy() {
        assert_eq!(
            cookie_header("abc", false),
            "authToken=abc; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400"
        );
        assert_eq!(
            cookie_header("", true),
            "authToken=; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400; Secure"
        );
    }
}
