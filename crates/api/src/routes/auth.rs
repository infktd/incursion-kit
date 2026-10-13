//! Session endpoints and EVE SSO login.
//!
//! - `GET /api/auth/whoami`: the account, its permission keys and characters.
//! - `GET /api/auth/logout`: clears the cookie (and unlinks every alt, like the legacy waitlist).
//! - `GET /api/auth/login_url?alt=&fc=&srp_admin=`: the EVE SSO URL (text). `state` is
//!   `srp_admin` / `alt` / `normal` plus a `:<nonce>` bound to a cookie ([`crate::auth::sso`]).
//! - `POST /api/auth/cb` `{code, state}`: completes a login and sets the session cookie. A banned
//!   character gets 403 with a JSON ban object as text; `alt` links the character to the current
//!   account; `srp_admin` keeps the session and sets its window character.
//! - `GET /api/auth/cb?code=&state=`: the SRP service-account setup (`state=srp_setup:<nonce>`),
//!   303 to `/fc/srp`. Unlike the legacy waitlist it requires `commanders-manage:admin`, and other
//!   states redirect to the site origin instead of a hardcoded `http://localhost:3000`.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Redirect};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::Character;
use wl_esi::LoginKind;

use crate::auth::sso::{self, SsoNonceCookie};
use crate::auth::{AuthenticatedAccount, Session, SetSessionCookie};
use crate::extract::JsonBody;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(whoami))
        .routes(routes!(logout))
        .routes(routes!(login_url))
        .routes(routes!(callback_get, callback))
}

#[derive(Serialize, Debug, ToSchema)]
struct WhoamiResponse {
    account_id: i64,
    /// The role's permission keys, sorted.
    #[schema(value_type = Vec<String>)]
    access: Vec<&'static str>,
    /// Main first, then alts.
    characters: Vec<Character>,
}

#[derive(sqlx::FromRow)]
struct CharacterRow {
    id: i64,
    name: String,
}

impl From<CharacterRow> for Character {
    fn from(row: CharacterRow) -> Self {
        Character {
            id: row.id,
            name: row.name,
            corporation_id: None,
        }
    }
}

/// The current account
///
/// The account id is the main character's id. `access` lists the permission keys of the
/// account's role.
#[utoipa::path(
    get,
    path = "/api/auth/whoami",
    tag = tags::AUTH,
    security(("session" = [])),
    responses((status = 200, description = "The logged-in account.", body = WhoamiResponse)),
)]
async fn whoami(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<WhoamiResponse>> {
    // A session for a character that no longer exists is a 500, as in the legacy waitlist.
    let main: CharacterRow = sqlx::query_as("SELECT id, name FROM `character` WHERE id = ?")
        .bind(account.id)
        .fetch_one(state.db())
        .await?;
    let alts: Vec<CharacterRow> = sqlx::query_as(
        "SELECT `character`.id, `character`.name FROM alt_character \
         JOIN `character` ON alt_character.alt_id = `character`.id \
         WHERE alt_character.account_id = ? ORDER BY alt_character.alt_id",
    )
    .bind(account.id)
    .fetch_all(state.db())
    .await?;

    let characters = std::iter::once(main)
        .chain(alts)
        .map(Character::from)
        .collect();
    Ok(Json(WhoamiResponse {
        account_id: account.id,
        access: account.access.iter().copied().collect(),
        characters,
    }))
}

/// Log out
///
/// Clears the session cookie. Like the legacy waitlist, logging out also unlinks every alt of
/// the account (and the account from any account it is an alt of).
#[utoipa::path(
    get,
    path = "/api/auth/logout",
    tag = tags::AUTH,
    security((), ("session" = [])),
    responses((
        status = 200,
        description = "Logged out (empty body).",
        headers(("set-cookie" = String, description = "Clears `authToken`.")),
    )),
)]
async fn logout(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
) -> ApiResult<SetSessionCookie> {
    if let Some(account) = account {
        sqlx::query("DELETE FROM alt_character WHERE account_id = ? OR alt_id = ?")
            .bind(account.id)
            .bind(account.id)
            .execute(state.db())
            .await?;
    }
    Ok(SetSessionCookie::cleared(state.secure_cookies()))
}

#[derive(Deserialize, Debug, Default)]
struct LoginUrlQuery {
    #[serde(default, deserialize_with = "flag")]
    alt: bool,
    #[serde(default, deserialize_with = "flag")]
    fc: bool,
    #[serde(default, deserialize_with = "flag")]
    srp_admin: bool,
}

/// A boolean query flag as the legacy server parsed it: `true`/`on`/`yes`/`1` or
/// `false`/`off`/`no`/`0` (any case); absent means false.
fn flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    let value = String::deserialize(deserializer)?;
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(serde::de::Error::custom(format!("invalid flag {value:?}"))),
    }
}

/// EVE SSO login URL
///
/// The URL to send the browser to, as text. Its OAuth `state` is `<kind>:<nonce>` where kind is
/// `normal`, `alt` or `srp_admin`; the nonce is also set in the short-lived `wlSsoNonce` cookie
/// (path `/api/auth`), and the callback only accepts a state whose nonce matches it.
#[utoipa::path(
    get,
    path = "/api/auth/login_url",
    tag = tags::AUTH,
    params(
        ("alt" = Option<bool>, Query, description = "Link the character as an alt of the current account."),
        ("fc" = Option<bool>, Query, description = "Also request the fleet-command ESI scopes."),
        ("srp_admin" = Option<bool>, Query, description = "SRP administrator login: keeps the session and uses the character for in-game windows."),
    ),
    responses((
        status = 200,
        description = "The EVE SSO URL.",
        body = String,
        content_type = "text/plain",
        headers(("set-cookie" = String, description = "`wlSsoNonce`: the login's nonce (HttpOnly, 15 minutes).")),
    )),
)]
async fn login_url(
    State(state): State<AppState>,
    Query(q): Query<LoginUrlQuery>,
) -> ApiResult<impl IntoResponse> {
    let kind = if q.srp_admin {
        LoginKind::SrpAdmin
    } else if q.alt {
        LoginKind::Alt
    } else {
        LoginKind::Normal
    };
    let (url, cookie) = sso::start_login(&state, kind, q.fc)?;
    Ok((
        cookie,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        url,
    ))
}

#[derive(Deserialize, Debug, ToSchema)]
struct CallbackData {
    /// The authorization code from EVE SSO.
    code: String,
    /// The OAuth `state` from EVE SSO (`<kind>:<nonce>`).
    state: Option<String>,
}

/// What a banned character is told at login (sent as JSON text with a 403).
#[derive(Serialize, Debug)]
struct PublicBanPayload {
    category: String,
    expires_at: Option<i64>,
    reason: Option<String>,
}

/// Exchanges the code (storing the tokens), refreshes the character's affiliation and refuses
/// banned characters, as the legacy callback did.
async fn complete_login(state: &AppState, code: &str) -> ApiResult<i64> {
    let auth = state.esi()?.process_authorization_code(code).await?;
    let character_id = auth.character_id;
    state.affiliation()?.update_character(character_id).await?;

    if let Some(ban) = crate::ban::character_bans(state.db(), character_id)
        .await?
        .and_then(|bans| bans.into_iter().next())
    {
        let payload = PublicBanPayload {
            category: ban.entity.map(|e| e.category).unwrap_or_default(),
            expires_at: ban.revoked_at,
            reason: ban.public_reason,
        };
        let json = serde_json::to_string(&payload).map_err(ApiError::internal)?;
        return Err(ApiError::Forbidden(json));
    }
    Ok(character_id)
}

/// Complete a login
///
/// Exchanges the authorization code, checks the `state` nonce against the `wlSsoNonce` cookie
/// and sets the session cookie. An `alt` login links the character to the current account; an
/// `srp_admin` login keeps the session and records the character for in-game windows. An
/// `srp_setup` login (requires `commanders-manage:admin`) stores the character as the SRP service
/// account and keeps the admin's session: frontends that compare the state exactly with
/// `"srp_setup"` send it here instead of to the `GET` variant.
#[utoipa::path(
    post,
    path = "/api/auth/cb",
    tag = tags::AUTH,
    security((), ("session" = [])),
    request_body = CallbackData,
    responses(
        (
            status = 200,
            description = "Logged in (empty body).",
            headers(("set-cookie" = String, description = "Sets `authToken` and clears `wlSsoNonce`.")),
        ),
        (status = 400, description = "Login state mismatch, or the character is a main and cannot be added as an alt.", body = String, content_type = "text/plain"),
        (status = 403, description = "The character is banned. The body is a JSON object as text: `{\"category\", \"expires_at\", \"reason\"}`.", body = String, content_type = "text/plain"),
    ),
)]
async fn callback(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    headers: HeaderMap,
    JsonBody(input): JsonBody<CallbackData>,
) -> ApiResult<impl IntoResponse> {
    let kind = sso::verify_state(&headers, input.state.as_deref().unwrap_or_default())?;
    if kind == Some(LoginKind::SrpSetup) {
        let account = account.ok_or(ApiError::Unauthenticated)?;
        account.require_access("commanders-manage:admin")?;
        let character_id = complete_login(&state, &input.code).await?;
        store_srp_service_account(&state, character_id).await?;
        let secure = state.secure_cookies();
        return Ok((
            SsoNonceCookie::cleared(secure)?,
            SetSessionCookie {
                token: state.session_key().encode(Session {
                    account_id: account.id,
                    window_character_id: account.window_character_id,
                })?,
                secure,
            },
        ));
    }
    let character_id = complete_login(&state, &input.code).await?;

    let session = match (kind, account) {
        (Some(LoginKind::Alt), Some(account)) => {
            if account.id != character_id {
                let is_admin: Option<i64> =
                    sqlx::query_scalar("SELECT character_id FROM admin WHERE character_id = ?")
                        .bind(character_id)
                        .fetch_optional(state.db())
                        .await?;
                if is_admin.is_some() {
                    return Err(ApiError::bad_request(
                        "Character is flagged as a main and cannot be added as an alt",
                    ));
                }
                sqlx::query("REPLACE INTO alt_character (account_id, alt_id) VALUES (?, ?)")
                    .bind(account.id)
                    .bind(character_id)
                    .execute(state.db())
                    .await?;
            }
            Session {
                account_id: account.id,
                window_character_id: None,
            }
        }
        // Keep the session; SRP pages open windows with this character's token.
        (Some(LoginKind::SrpAdmin), Some(account)) => Session {
            account_id: account.id,
            window_character_id: Some(character_id),
        },
        _ => Session {
            account_id: character_id,
            window_character_id: None,
        },
    };

    let secure = state.secure_cookies();
    Ok((
        SsoNonceCookie::cleared(secure)?,
        SetSessionCookie {
            token: state.session_key().encode(session)?,
            secure,
        },
    ))
}

#[derive(Deserialize, Debug)]
struct CallbackQuery {
    code: String,
    state: Option<String>,
}

/// SSO redirect target (SRP service account)
///
/// With `state=srp_setup:<nonce>` this stores the logged-in character as the SRP service account
/// (requires `commanders-manage:admin`) and redirects to `/fc/srp`. Any other login redirects to
/// the site origin.
#[utoipa::path(
    get,
    path = "/api/auth/cb",
    tag = tags::AUTH,
    security((), ("session" = ["commanders-manage:admin"])),
    params(
        ("code" = String, Query, description = "The authorization code from EVE SSO."),
        ("state" = Option<String>, Query, description = "The OAuth state (`<kind>:<nonce>`)."),
    ),
    responses(
        (status = 303, description = "Redirect to `/fc/srp` (SRP setup) or the site origin.", headers(("location" = String))),
        (status = 400, description = "Login state mismatch, or the character has no corporation.", body = String, content_type = "text/plain"),
        (status = 401, description = "An SRP setup login without a session or without `commanders-manage:admin`.", body = String, content_type = "text/plain"),
    ),
)]
async fn callback_get(
    State(state): State<AppState>,
    account: Option<AuthenticatedAccount>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> ApiResult<impl IntoResponse> {
    let raw_state = q.state.as_deref().unwrap_or_default();
    let is_srp_setup = LoginKind::from_state(raw_state) == Some(LoginKind::SrpSetup);
    if is_srp_setup {
        // The legacy endpoint was open to anyone; it replaces the SRP service account.
        account
            .as_ref()
            .ok_or(ApiError::Unauthenticated)?
            .require_access("commanders-manage:admin")?;
    }
    sso::verify_state(&headers, raw_state)?;
    let character_id = complete_login(&state, &q.code).await?;
    let cleared = SsoNonceCookie::cleared(state.secure_cookies())?;

    if !is_srp_setup {
        return Ok((cleared, Redirect::to(&site_origin(&state))));
    }

    store_srp_service_account(&state, character_id).await?;
    Ok((cleared, Redirect::to("/fc/srp")))
}

/// Stores a freshly logged-in character (tokens just saved by [`complete_login`]) as the SRP
/// service account.
async fn store_srp_service_account(state: &AppState, character_id: i64) -> ApiResult<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        name: String,
        corporation_id: Option<i64>,
        access_token: String,
        refresh_token: String,
    }
    let row: Row = sqlx::query_as(
        "SELECT c.name, c.corporation_id, a.access_token, r.refresh_token
         FROM `character` c
         JOIN access_token a ON a.character_id = c.id
         JOIN refresh_token r ON r.character_id = c.id
         WHERE c.id = ?",
    )
    .bind(character_id)
    .fetch_one(state.db())
    .await?;
    let corporation_id = row
        .corporation_id
        .ok_or_else(|| ApiError::bad_request("Character has no corporation ID"))?;

    wl_srp::service_account::store_service_account_tokens(
        state.db(),
        &wl_srp::service_account::ServiceAccountTokens {
            character_id,
            character_name: &row.name,
            corporation_id,
            wallet_id: wl_srp::service_account::DEFAULT_WALLET_DIVISION,
            access_token: &row.access_token,
            refresh_token: &row.refresh_token,
            // The legacy waitlist assumed a fresh token is valid for 20 minutes.
            expires: chrono::Utc::now().timestamp() + 1200,
            scopes: wl_srp::service_account::SERVICE_ACCOUNT_SCOPES,
        },
    )
    .await?;

    Ok(())
}

/// The public site's origin with a trailing slash (`https://example.org/`), from the SSO callback
/// URL `esi.url`.
fn site_origin(state: &AppState) -> String {
    url::Url::parse(&state.config().esi.url)
        .map(|url| format!("{}/", url.origin().ascii_serialization()))
        .unwrap_or_else(|_| "/".to_string())
}
