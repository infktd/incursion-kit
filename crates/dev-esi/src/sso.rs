//! EVE SSO: the login page, the token endpoint and the key set.
//!
//! - `GET /v2/oauth/authorize?response_type=code&client_id=&redirect_uri=&scope=&state=
//!   [&code_challenge=&code_challenge_method=S256]`: a page listing the seeded characters. Picking
//!   one (`POST` of the same parameters plus `character_id`, or a `GET` with `character_id` for
//!   scripts) redirects to `redirect_uri?code=…&state=…` with `state` unchanged; "Cancel" sends
//!   `error=access_denied`.
//! - `POST /v2/oauth/token` (form): `grant_type=authorization_code` (client secret over HTTP Basic
//!   or `client_secret`, or a public client with the PKCE `code_verifier`) and
//!   `grant_type=refresh_token` (optional `scope`, at most the granted ones). Refresh tokens are
//!   short opaque strings, like EVE's, and rotate: each can be used once. Errors are OAuth's
//!   (`invalid_grant`, `invalid_client`, ...).
//! - `GET /oauth/jwks`: the public key the access tokens are signed with.
//!
//! The `/oauth/*` spellings of the first two work too.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use base64::Engine as _;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::jwt::{KEY_ID, pkce_s256, random_token};
use crate::seed::SsoClient;
use crate::world::{AuthCode, RefreshGrant, World};
use crate::{DevEsi, escape};

/// Authorization codes live this long (seconds).
const CODE_LIFETIME: i64 = 300;

pub fn routes() -> Router<DevEsi> {
    Router::new()
        .route(
            "/v2/oauth/authorize",
            get(authorize_page).post(authorize_pick),
        )
        .route("/oauth/authorize", get(authorize_page).post(authorize_pick))
        .route("/v2/oauth/token", axum::routing::post(token))
        .route("/oauth/token", axum::routing::post(token))
        .route("/oauth/jwks", get(jwks))
}

#[derive(Debug, Deserialize)]
struct AuthorizeParams {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    scope: String,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    /// The character picked on the page (or given directly by a script).
    character_id: Option<i64>,
    /// "Cancel" on the page.
    deny: Option<String>,
}

fn error_page(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!doctype html><meta charset=utf-8><title>dev-esi: login error</title>\
             <body style=\"font-family:system-ui;margin:2em\"><h1>Login error</h1><p>{}</p>",
            escape(message)
        )),
    )
        .into_response()
}

/// The scopes of a `scope` parameter, de-duplicated, in order.
fn split_scopes(scope: &str) -> Vec<String> {
    let mut scopes: Vec<String> = Vec::new();
    for s in scope.split_whitespace() {
        if !scopes.iter().any(|known| known == s) {
            scopes.push(s.to_string());
        }
    }
    scopes
}

/// Check the client and callback of an authorize request.
fn check_authorize(world: &World, params: &AuthorizeParams) -> Result<(), String> {
    if params.response_type != "code" {
        return Err("response_type must be \"code\"".into());
    }
    let client = world
        .settings
        .clients
        .iter()
        .find(|c| c.client_id == params.client_id)
        .ok_or_else(|| format!("unknown client_id {:?}", params.client_id))?;
    if url::Url::parse(&params.redirect_uri).is_err() {
        return Err(format!("invalid redirect_uri {:?}", params.redirect_uri));
    }
    if !client.redirect_uris.is_empty() && !client.redirect_uris.contains(&params.redirect_uri) {
        return Err(format!(
            "redirect_uri {:?} is not registered for {}",
            params.redirect_uri, client.client_id
        ));
    }
    if params
        .code_challenge_method
        .as_deref()
        .is_some_and(|m| m != "S256" && m != "plain")
    {
        return Err("code_challenge_method must be S256 or plain".into());
    }
    Ok(())
}

/// `redirect_uri` with these query parameters added.
fn callback(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(redirect_uri).expect("checked by check_authorize");
    {
        let mut query = url.query_pairs_mut();
        for (k, v) in params {
            query.append_pair(k, v);
        }
    }
    url.to_string()
}

async fn authorize_page(
    State(dev): State<DevEsi>,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if params.character_id.is_some() || params.deny.is_some() {
        return authorize_pick(State(dev), Form(params)).await;
    }
    let world = dev.world();
    if let Err(message) = check_authorize(&world, &params) {
        return error_page(&message);
    }
    let hidden = |name: &str, value: Option<&str>| {
        value.map_or(String::new(), |v| {
            format!(
                "<input type=hidden name=\"{name}\" value=\"{}\">",
                escape(v)
            )
        })
    };
    let fields = [
        hidden("response_type", Some(&params.response_type)),
        hidden("client_id", Some(&params.client_id)),
        hidden("redirect_uri", Some(&params.redirect_uri)),
        hidden("scope", Some(&params.scope)),
        hidden("state", params.state.as_deref()),
        hidden("code_challenge", params.code_challenge.as_deref()),
        hidden(
            "code_challenge_method",
            params.code_challenge_method.as_deref(),
        ),
    ]
    .concat();

    let mut rows = String::new();
    let mut characters: Vec<_> = world.characters.values().collect();
    characters.sort_by_key(|c| (c.corporation_id, c.name.clone()));
    for c in characters {
        let corp = world.corporations.get(&c.corporation_id);
        let alliance = world
            .alliance_of(c)
            .and_then(|id| world.alliances.get(&id))
            .map(|a| format!(" &middot; {}", escape(&a.name)))
            .unwrap_or_default();
        rows.push_str(&format!(
            "<li><button name=character_id value=\"{id}\"><b>{name}</b> \
             <small>{id}</small><br><span>[{ticker}] {corp}{alliance}</span>\
             {notes}</button></li>",
            id = c.id,
            name = escape(&c.name),
            ticker = escape(corp.map_or("", |c| c.ticker.as_str())),
            corp = escape(corp.map_or("", |c| c.name.as_str())),
            notes = if c.notes.is_empty() {
                String::new()
            } else {
                format!("<br><i>{}</i>", escape(&c.notes))
            },
        ));
    }
    let scopes: String = split_scopes(&params.scope)
        .iter()
        .map(|s| format!("<code>{}</code> ", escape(s)))
        .collect();
    Html(format!(
        r#"<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width">
<title>dev-esi: log in</title>
<style>
body{{font-family:system-ui,sans-serif;margin:0 auto;max-width:44em;padding:1em;color:#222}}
ul{{list-style:none;padding:0}} li{{margin:.4em 0}}
button[name=character_id]{{width:100%;text-align:left;padding:.6em .8em;border:1px solid #bbb;
border-radius:6px;background:#fafafa;cursor:pointer;font:inherit}}
button[name=character_id]:hover{{background:#eef4ff;border-color:#69c}}
small,span,i{{color:#666}} code{{background:#f0f0f0;padding:0 .2em}}
.warn{{background:#fff4d6;border:1px solid #e0b84f;padding:.5em .8em;border-radius:6px}}
</style>
<h1>Log in to <code>{client}</code></h1>
<p class=warn>dev-esi: a fake EVE SSO for development. Pick any character; no password.</p>
<p>Requested scopes: {scopes}</p>
<form method=post action=authorize>{fields}<ul>{rows}</ul>
<button name=deny value=1>Cancel</button></form>"#,
        client = escape(&params.client_id),
    ))
    .into_response()
}

async fn authorize_pick(
    State(dev): State<DevEsi>,
    Form(params): Form<AuthorizeParams>,
) -> Response {
    let mut world = dev.world();
    if let Err(message) = check_authorize(&world, &params) {
        return error_page(&message);
    }
    let state = params.state.as_deref();
    let with_state = |mut pairs: Vec<(&'static str, String)>| {
        if let Some(state) = state {
            pairs.push(("state", state.to_string()));
        }
        pairs
    };
    if params.deny.is_some() {
        let pairs = with_state(vec![("error", "access_denied".to_string())]);
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        return Redirect::to(&callback(&params.redirect_uri, &pairs)).into_response();
    }
    let Some(character_id) = params.character_id else {
        return error_page("no character picked");
    };
    let Ok(character) = world.character(character_id) else {
        return error_page(&format!("unknown character {character_id}"));
    };
    let name = character.name.clone();
    let scopes = split_scopes(&params.scope);
    let code = random_token(24);
    world
        .sso
        .codes
        .retain(|_, c| c.expires_at > Utc::now().timestamp());
    world.sso.codes.insert(
        code.clone(),
        AuthCode {
            client_id: params.client_id.clone(),
            redirect_uri: params.redirect_uri.clone(),
            character_id,
            scopes: scopes.clone(),
            challenge: params.code_challenge.clone().map(|challenge| {
                (
                    params
                        .code_challenge_method
                        .clone()
                        .unwrap_or_else(|| "plain".into()),
                    challenge,
                )
            }),
            expires_at: Utc::now().timestamp() + CODE_LIFETIME,
        },
    );
    world.event(
        "login",
        Some(character_id),
        None,
        format!(
            "{name} logged in to {} ({})",
            params.client_id,
            scopes.join(" ")
        ),
    );
    let pairs = with_state(vec![("code", code)]);
    let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    // EVE answers the login form with a 302.
    (
        StatusCode::FOUND,
        [(header::LOCATION, callback(&params.redirect_uri, &pairs))],
    )
        .into_response()
}

/// An OAuth error response.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"error": error, "error_description": description})),
    )
        .into_response()
}

fn invalid_grant(description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

/// Client credentials from HTTP Basic (form-urlencoded parts, as OAuth specifies) or the body.
fn client_credentials(
    headers: &HeaderMap,
    form: &HashMap<String, String>,
) -> Option<(String, Option<String>)> {
    let decode = |s: &str| {
        url::form_urlencoded::parse(format!("x={s}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default()
    };
    if let Some(basic) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(basic.trim())
            .ok()?;
        let raw = String::from_utf8(raw).ok()?;
        let (id, secret) = raw.split_once(':')?;
        return Some((decode(id), Some(decode(secret))));
    }
    let id = form.get("client_id")?.clone();
    Some((id, form.get("client_secret").cloned()))
}

async fn token(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let invalid_client = || {
        oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "Client authentication failed",
        )
    };
    let Some((client_id, secret)) = client_credentials(&headers, &form) else {
        return invalid_client();
    };
    let mut world = dev.world();
    let Some(client) = world
        .settings
        .clients
        .iter()
        .find(|c| c.client_id == client_id)
        .cloned()
    else {
        return invalid_client();
    };
    // A confidential client must prove its secret whenever it sends one; a public client (or
    // one that sends none) relies on PKCE for codes.
    let authenticated = match (&client.client_secret, &secret) {
        (Some(expected), Some(given)) if expected == given => true,
        (_, Some(_)) => return invalid_client(),
        (_, None) => false,
    };

    let grant_type = form.get("grant_type").map(String::as_str).unwrap_or("");
    let (character_id, scopes, grant_scopes) = match grant_type {
        "authorization_code" => {
            let code = form.get("code").cloned().unwrap_or_default();
            let Some(grant) = world.sso.codes.remove(&code) else {
                return invalid_grant("Authorization code not found");
            };
            if grant.expires_at < Utc::now().timestamp() {
                return invalid_grant("Authorization code expired");
            }
            if grant.client_id != client.client_id {
                return invalid_grant("Authorization code was issued to another client");
            }
            if form
                .get("redirect_uri")
                .is_some_and(|uri| *uri != grant.redirect_uri)
            {
                return invalid_grant("redirect_uri does not match the authorization request");
            }
            match (&grant.challenge, form.get("code_verifier")) {
                (Some((method, challenge)), Some(verifier)) => {
                    let computed = if method == "S256" {
                        pkce_s256(verifier)
                    } else {
                        verifier.clone()
                    };
                    if computed != *challenge {
                        return invalid_grant("PKCE verification failed");
                    }
                }
                (Some(_), None) => return invalid_grant("code_verifier is required"),
                (None, _) if !authenticated => return invalid_client(),
                (None, _) => {}
            }
            (grant.character_id, grant.scopes.clone(), grant.scopes)
        }
        "refresh_token" => {
            let raw = form.get("refresh_token").cloned().unwrap_or_default();
            let Some(grant) = world
                .sso
                .refresh_tokens
                .get(&raw)
                .filter(|g| {
                    g.client_id == client.client_id
                        && world.characters.contains_key(&g.character_id)
                })
                .cloned()
            else {
                return invalid_grant("Invalid refresh token. Token missing/expired.");
            };
            let requested = split_scopes(form.get("scope").map(String::as_str).unwrap_or(""));
            if let Some(extra) = requested.iter().find(|s| !grant.scopes.contains(s)) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_scope",
                    &format!("The scope {extra} was not granted to this refresh token"),
                );
            }
            // Refresh tokens rotate: this one is spent.
            world.sso.refresh_tokens.remove(&raw);
            let scopes = if requested.is_empty() {
                grant.scopes.clone()
            } else {
                requested
            };
            (grant.character_id, scopes, grant.scopes)
        }
        "" => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "grant_type is required",
            );
        }
        other => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                &format!("grant_type {other} is not supported"),
            );
        }
    };

    let body = issue_tokens(
        &dev,
        &mut world,
        &client,
        character_id,
        &scopes,
        &grant_scopes,
    );
    dev.save_grants(&world);
    let detail = format!(
        "{grant_type} tokens for {} ({})",
        world.character_name(character_id),
        scopes.join(" ")
    );
    world.event("token", Some(character_id), None, detail);
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(body),
    )
        .into_response()
}

/// The character of a token's `sub` (`CHARACTER:EVE:<id>`).
pub(crate) fn character_of(claims: &Value) -> Option<i64> {
    claims["sub"]
        .as_str()?
        .strip_prefix("CHARACTER:EVE:")?
        .parse()
        .ok()
}

/// A token response: an access token with `scopes` and a refresh token for `grant_scopes`.
fn issue_tokens(
    dev: &DevEsi,
    world: &mut World,
    client: &SsoClient,
    character_id: i64,
    scopes: &[String],
    grant_scopes: &[String],
) -> Value {
    let character = &world.characters[&character_id];
    let now = Utc::now().timestamp();
    let lifetime = world.settings.access_token_lifetime_secs;
    let mut claims = json!({
        "jti": random_token(16),
        "kid": KEY_ID,
        "sub": format!("CHARACTER:EVE:{character_id}"),
        "azp": client.client_id,
        "tenant": "tranquility",
        "tier": "live",
        "region": "world",
        "aud": [client.client_id, "EVE Online"],
        "name": character.name,
        "owner": world.owner_hash(character),
        "exp": now + i64::try_from(lifetime).unwrap_or(1199),
        "iat": now,
        "iss": world.settings.issuer,
    });
    // EVE sends one scope as a string and several as an array.
    match scopes {
        [] => {}
        [one] => claims["scp"] = json!(one),
        many => claims["scp"] = json!(many),
    }
    let access_token = dev.signer().sign(&claims);
    let refresh_token = random_token(24);
    world.sso.refresh_tokens.insert(
        refresh_token.clone(),
        RefreshGrant {
            character_id,
            client_id: client.client_id.clone(),
            scopes: grant_scopes.to_vec(),
            issued_at: now,
        },
    );
    json!({
        "access_token": access_token,
        "expires_in": lifetime,
        "token_type": "Bearer",
        "refresh_token": refresh_token,
    })
}

async fn jwks(State(dev): State<DevEsi>) -> Json<Value> {
    Json(dev.signer().jwks())
}
