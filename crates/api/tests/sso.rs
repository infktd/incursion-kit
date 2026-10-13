//! EVE SSO: `GET /api/auth/login_url`, `POST /api/auth/cb` and `GET /api/auth/cb`.

mod common;
mod support;

use axum::body::Body;
use axum::http::{Method, StatusCode, header};
use common::{Login, TestApp};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::{ALL_SCOPES, Env, mock_code_exchange};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};
use wl_api::auth::sso::{NONCE_COOKIE, STATE_MISMATCH};

const NONCE: &str = "0123456789abcdef";

/// `login` (if any) plus the SSO nonce cookie.
fn cookies(login: Option<&Login>, nonce: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(login) = login {
        parts.push(login.cookie.clone());
    }
    if let Some(nonce) = nonce {
        parts.push(format!("{NONCE_COOKIE}={nonce}"));
    }
    parts.join("; ")
}

async fn post_cb(
    env: &Env,
    login: Option<&Login>,
    nonce: Option<&str>,
    body: Value,
) -> common::TestResponse {
    env.request(
        TestApp::build(Method::POST, "/api/auth/cb", None)
            .header(header::COOKIE, cookies(login, nonce))
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn get_cb(
    env: &Env,
    login: Option<&Login>,
    nonce: Option<&str>,
    query: &str,
) -> common::TestResponse {
    env.request(
        TestApp::build(Method::GET, &format!("/api/auth/cb?{query}"), None)
            .header(header::COOKIE, cookies(login, nonce))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// The session in a response's `authToken` cookie.
fn session(env: &Env, res: &common::TestResponse) -> wl_api::auth::Session {
    let cookie = res
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|c| c.starts_with("authToken="))
        .expect("session cookie");
    assert!(
        cookie.ends_with("; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400"),
        "{cookie}"
    );
    let token = cookie["authToken=".len()..].split(';').next().unwrap();
    env.state.session_key().decode(token).unwrap()
}

fn set_cookies(res: &common::TestResponse) -> Vec<String> {
    res.headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("no {name} in {url}"))
}

#[tokio::test]
async fn login_url_variants_carry_a_nonce() {
    let Some(env) = Env::new().await else { return };
    for (query, state, scopes) in [
        (
            "",
            "normal",
            "publicData esi-skills.read_skills.v1 esi-clones.read_implants.v1",
        ),
        (
            "?alt=true",
            "alt",
            "publicData esi-skills.read_skills.v1 esi-clones.read_implants.v1",
        ),
        (
            "?fc=yes&alt=false",
            "normal",
            "publicData esi-skills.read_skills.v1 esi-clones.read_implants.v1 esi-fleets.read_fleet.v1 \
             esi-fleets.write_fleet.v1 esi-ui.open_window.v1 esi-search.search_structures.v1",
        ),
        (
            "?alt=1&srp_admin=on",
            "srp_admin",
            "publicData esi-skills.read_skills.v1 esi-clones.read_implants.v1 esi-ui.open_window.v1",
        ),
    ] {
        let res = env.get(&format!("/api/auth/login_url{query}"), None).await;
        assert_eq!(res.status, StatusCode::OK, "{query}");
        assert_eq!(
            res.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        let url = res.text();
        assert!(
            url.starts_with(&format!("{}/v2/oauth/authorize?", env.esi.uri())),
            "{url}"
        );
        assert_eq!(query_param(url, "scope"), scopes, "{query}");
        assert_eq!(
            query_param(url, "redirect_uri"),
            "http://waitlist.test/auth/cb"
        );
        let full_state = query_param(url, "state");
        let (kind, nonce) = full_state.split_once(':').unwrap();
        assert_eq!(kind, state);
        assert_eq!(nonce.len(), 32);
        assert_eq!(
            set_cookies(&res),
            [format!(
                "{NONCE_COOKIE}={nonce}; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=900"
            )]
        );
    }
    // Unparseable query value: legacy answered "no route matches" (404).
    let res = env.get("/api/auth/login_url?alt=maybe", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn callback_logs_in_and_clears_the_nonce() {
    let Some(env) = Env::new().await else { return };
    mock_code_exchange(&env.esi, 5001, &ALL_SCOPES[..3]).await;
    env.mock_character(5001, "Pilot 5001").await;

    let res = post_cb(
        &env,
        None,
        Some(NONCE),
        json!({"code": "c", "state": format!("normal:{NONCE}")}),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.text(), "");
    assert!(set_cookies(&res).contains(&format!(
        "{NONCE_COOKIE}=; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=0"
    )));
    let s = session(&env, &res);
    assert_eq!((s.account_id, s.window_character_id), (5001, None));
    let corporation: Option<i64> =
        sqlx::query_scalar("SELECT corporation_id FROM `character` WHERE id = 5001")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(corporation, Some(support::CORPORATION_ID));
    let scopes: String =
        sqlx::query_scalar("SELECT scopes FROM refresh_token WHERE character_id = 5001")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert!(scopes.contains("esi-skills.read_skills.v1"));

    // A logged-in user re-authenticating with a normal login switches to that character.
    let other = env.pilot(6001, None).await;
    let res = post_cb(
        &env,
        Some(&other),
        Some(NONCE),
        json!({"code": "c", "state": format!("normal:{NONCE}")}),
    )
    .await;
    assert_eq!(session(&env, &res).account_id, 5001);
}

#[tokio::test]
async fn callback_rejects_a_missing_or_foreign_nonce() {
    let Some(env) = Env::new().await else { return };
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&env.esi)
        .await;
    for (nonce, state) in [
        (None, format!("normal:{NONCE}")),
        (Some(NONCE), "normal".to_string()),
        (Some(NONCE), "alt:ffff".to_string()),
    ] {
        let res = post_cb(&env, None, nonce, json!({"code": "c", "state": state})).await;
        assert_eq!(
            (res.status, res.text()),
            (StatusCode::BAD_REQUEST, STATE_MISMATCH)
        );
        assert!(set_cookies(&res).is_empty());
    }
    let res = post_cb(&env, None, Some(NONCE), json!({"code": "c"})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = post_cb(&env, None, Some(NONCE), json!({"state": "x"})).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn alt_login_links_to_the_current_account() {
    let Some(env) = Env::new().await else { return };
    let main = env.pilot(1001, None).await;
    mock_code_exchange(&env.esi, 5001, &ALL_SCOPES[..3]).await;
    env.mock_character(5001, "Pilot 5001").await;
    let state = json!({"code": "c", "state": format!("alt:{NONCE}")});

    let res = post_cb(&env, Some(&main), Some(NONCE), state.clone()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let s = session(&env, &res);
    assert_eq!((s.account_id, s.window_character_id), (1001, None));
    let res = env.get("/api/auth/whoami", Some(&main)).await;
    assert_eq!(
        res.json::<Value>()["characters"],
        json!([{"id": 1001, "name": "Pilot 1001"}, {"id": 5001, "name": "Pilot 5001"}])
    );

    // Logged out, an alt login is a normal login.
    let res = post_cb(&env, None, Some(NONCE), state.clone()).await;
    assert_eq!(session(&env, &res).account_id, 5001);

    // Characters with a role are mains.
    env.set_role(5001, "fc").await;
    let res = post_cb(&env, Some(&main), Some(NONCE), state).await;
    assert_eq!(
        (res.status, res.text()),
        (
            StatusCode::BAD_REQUEST,
            "Character is flagged as a main and cannot be added as an alt"
        )
    );
}

#[tokio::test]
async fn srp_admin_login_keeps_the_session() {
    let Some(env) = Env::new().await else { return };
    let admin = env.pilot(1001, Some("admin")).await;
    mock_code_exchange(&env.esi, 5001, &ALL_SCOPES).await;
    env.mock_character(5001, "Pilot 5001").await;
    let body = json!({"code": "c", "state": format!("srp_admin:{NONCE}")});

    let res = post_cb(&env, Some(&admin), Some(NONCE), body.clone()).await;
    let s = session(&env, &res);
    assert_eq!((s.account_id, s.window_character_id), (1001, Some(5001)));
    let res = post_cb(&env, None, Some(NONCE), body).await;
    let s = session(&env, &res);
    assert_eq!((s.account_id, s.window_character_id), (5001, None));
}

#[tokio::test]
async fn banned_characters_get_the_ban_as_json() {
    let Some(env) = Env::new().await else { return };
    env.pilot(1001, Some("fc")).await;
    mock_code_exchange(&env.esi, 5001, &ALL_SCOPES[..3]).await;
    env.mock_character(5001, "Pilot 5001").await;
    sqlx::query(
        "INSERT INTO ban (entity_id, entity_type, issued_at, issued_by, public_reason, reason, revoked_at)
         VALUES (?, 'Corporation', 0, 1001, 'be nice', 'secret', 4102444800)",
    )
    .bind(support::CORPORATION_ID)
    .execute(env.pool())
    .await
    .unwrap();

    let res = post_cb(
        &env,
        None,
        Some(NONCE),
        json!({"code": "c", "state": format!("normal:{NONCE}")}),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert_eq!(
        res.json::<Value>(),
        json!({"category": "Corporation", "expires_at": 4102444800i64, "reason": "be nice"})
    );
    assert!(
        !set_cookies(&res)
            .iter()
            .any(|c| c.starts_with("authToken="))
    );
}

#[tokio::test]
async fn srp_setup_requires_an_admin_and_stores_the_service_account() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(1001, Some("fc+")).await;
    let admin = env.pilot(1002, Some("admin")).await;
    mock_code_exchange(
        &env.esi,
        5001,
        &["publicData", "esi-wallet.read_corporation_wallets.v1"],
    )
    .await;
    env.mock_character(5001, "Pilot 5001").await;
    let query = format!("code=c&state=srp_setup:{NONCE}");

    let res = get_cb(&env, None, Some(NONCE), &query).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "401 Authorization Required")
    );
    let res = get_cb(&env, Some(&fc), Some(NONCE), &query).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = get_cb(&env, Some(&admin), None, &query).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::BAD_REQUEST, STATE_MISMATCH)
    );
    let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM srp_service_account")
        .fetch_one(env.pool())
        .await
        .unwrap();
    assert_eq!(accounts, 0);

    let res = get_cb(&env, Some(&admin), Some(NONCE), &query).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.text());
    assert_eq!(res.header("location"), Some("/fc/srp"));
    let (name, corporation, wallet, scopes): (String, i64, i32, String) = sqlx::query_as(
        "SELECT character_name, corporation_id, wallet_id, scopes FROM srp_service_account WHERE character_id = 5001",
    )
    .fetch_one(env.pool())
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), corporation, wallet, scopes.as_str()),
        (
            "Pilot 5001",
            support::CORPORATION_ID,
            1,
            "esi-publicdata.v1 esi-wallet.read_corporation_wallets.v1"
        )
    );
    // No session is issued.
    assert!(
        !set_cookies(&res)
            .iter()
            .any(|c| c.starts_with("authToken="))
    );

    // Any other state just redirects to the site.
    let res = get_cb(
        &env,
        None,
        Some(NONCE),
        &format!("code=c&state=normal:{NONCE}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), Some("http://waitlist.test/"));
}

/// Frontends that compare the state exactly with `"srp_setup"` miss the nonce suffix and send
/// the SRP setup login to `POST /api/auth/cb`: it must store the service account and keep the
/// admin's session rather than logging the admin in as the service character.
#[tokio::test]
async fn srp_setup_via_post_stores_the_service_account_and_keeps_the_session() {
    let Some(env) = Env::new().await else { return };
    let fc = env.pilot(1001, Some("fc+")).await;
    let admin = env.pilot(1002, Some("admin")).await;
    mock_code_exchange(
        &env.esi,
        5001,
        &["publicData", "esi-wallet.read_corporation_wallets.v1"],
    )
    .await;
    env.mock_character(5001, "Pilot 5001").await;
    let body = json!({"code": "c", "state": format!("srp_setup:{NONCE}")});

    let res = post_cb(&env, None, Some(NONCE), body.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = post_cb(&env, Some(&fc), Some(NONCE), body.clone()).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );

    let res = post_cb(&env, Some(&admin), Some(NONCE), body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let s = session(&env, &res);
    assert_eq!((s.account_id, s.window_character_id), (1002, None));
    let name: String = sqlx::query_scalar(
        "SELECT character_name FROM srp_service_account WHERE character_id = 5001",
    )
    .fetch_one(env.pool())
    .await
    .unwrap();
    assert_eq!(name, "Pilot 5001");
}
