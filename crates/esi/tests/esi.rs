//! wl-esi against a mock ESI and EVE SSO (wiremock).
//!
//! Tests that store tokens need MySQL (`TEST_DATABASE_URL`, see `wl_db::test_support`) and pass
//! as skipped without it; the rest use a pool that never connects.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use wl_db::test_support::TestDb;
use wl_esi::{Esi, EsiError, EsiErrorExt as _, EsiOptions, LoginKind, Scope};

const CLIENT_ID: &str = "client-id";
const CLIENT_SECRET: &str = "client-secret";
const USER_AGENT: &str = "incursion-kit-tests/1.0 (+https://waitlist.example)";

const BASE_SCOPES: [&str; 3] = [
    "publicData",
    "esi-skills.read_skills.v1",
    "esi-clones.read_implants.v1",
];
const FC_SCOPES: [&str; 2] = ["esi-fleets.read_fleet.v1", "esi-fleets.write_fleet.v1"];

fn options(server: &MockServer) -> EsiOptions {
    EsiOptions {
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        redirect_url: "https://waitlist.example/auth/cb".into(),
        user_agent: USER_AGENT.into(),
        esi_url: server.uri(),
        sso_authorize_url: format!("{}/v2/oauth/authorize", server.uri()),
        sso_token_url: format!("{}/v2/oauth/token", server.uri()),
    }
}

/// An `Esi` whose database is never reached.
fn esi_without_db(server: &MockServer) -> Esi {
    let pool = sqlx::MySqlPool::connect_lazy("mysql://nobody@127.0.0.1:1/none").unwrap();
    Esi::with_options(options(server), pool).unwrap()
}

/// An unsigned EVE-style access token.
fn jwt(character_id: i64, name: &str, scopes: &[&str]) -> String {
    let b64 = |v: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
    };
    format!(
        "{}.{}.sig",
        b64(&json!({"alg": "RS256", "kid": "JWT-Signature-Key"})),
        b64(&json!({
            "sub": format!("CHARACTER:EVE:{character_id}"),
            "name": name,
            "owner": "owner",
            "scp": scopes,
            "aud": [CLIENT_ID, "EVE Online"],
            "iss": "https://login.eveonline.com",
        }))
    )
}

fn token_body(access_token: &str, refresh_token: &str) -> serde_json::Value {
    json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": 1199,
        "refresh_token": refresh_token,
    })
}

fn basic_auth() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    )
}

fn token_endpoint() -> wiremock::MockBuilder {
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token"))
        .and(header("authorization", basic_auth().as_str()))
}

#[tokio::test]
async fn login_urls_use_the_legacy_scopes_and_state_without_pkce() {
    let server = MockServer::start().await;
    let esi = esi_without_db(&server);

    let url = esi.login_url(LoginKind::Alt, true);
    assert!(
        url.starts_with(&format!("{}/v2/oauth/authorize?", server.uri())),
        "{url}"
    );
    assert!(url.contains("response_type=code"), "{url}");
    assert!(url.contains("client_id=client-id"), "{url}");
    assert!(
        url.contains("state=alt&") || url.ends_with("state=alt"),
        "{url}"
    );
    assert!(
        url.contains("redirect_uri=https%3A%2F%2Fwaitlist.example%2Fauth%2Fcb"),
        "{url}"
    );
    assert!(
        url.contains(
            "scope=publicData+esi-skills.read_skills.v1+esi-clones.read_implants.v1+\
             esi-fleets.read_fleet.v1+esi-fleets.write_fleet.v1+esi-ui.open_window.v1+\
             esi-search.search_structures.v1"
        ),
        "{url}"
    );
    assert!(!url.contains("code_challenge"), "{url}");
    assert!(!url.contains(CLIENT_SECRET), "{url}");

    let srp = esi.login_url(LoginKind::SrpSetup, false);
    assert!(srp.contains("state=srp_setup"), "{srp}");
    assert!(
        srp.contains("scope=publicData+esi-wallet.read_corporation_wallets.v1"),
        "{srp}"
    );
    let custom = esi.login_url_with_state(&[Scope::PublicData], "normal:nonce");
    assert!(custom.contains("state=normal%3Anonce"), "{custom}");
}

#[tokio::test]
async fn public_endpoints_send_the_user_agent_and_map_legacy_shapes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/incursions"))
        .and(header("user-agent", USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "constellation_id": 20000001,
            "faction_id": 500019,
            "has_boss": true,
            "infested_solar_systems": [30000001, 30000002],
            "influence": 0.25,
            "staging_solar_system_id": 30000001,
            "state": "established",
            "type": "Incursion"
        }])))
        .expect(1)
        .mount(&server)
        .await;
    let esi = esi_without_db(&server);
    let incursions = esi.incursions().await.unwrap();
    assert_eq!(incursions.len(), 1);
    assert_eq!(incursions[0].state, "established");
    assert_eq!(incursions[0].type_, "Incursion");
    assert_eq!(
        serde_json::to_value(&incursions[0]).unwrap()["type"],
        "Incursion"
    );
}

#[tokio::test]
async fn public_errors_are_statuses_and_bulk_calls_are_chunked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/characters/1"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": "Character not found"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(|req: &Request| {
            let ids: Vec<i64> = serde_json::from_slice(&req.body).unwrap();
            assert!(ids.len() <= 1000);
            let names: Vec<_> = ids
                .iter()
                .map(|id| json!({"id": id, "name": format!("Pilot {id}"), "category": "character"}))
                .collect();
            ResponseTemplate::new(200).set_body_json(names)
        })
        .expect(2)
        .mount(&server)
        .await;
    let esi = esi_without_db(&server);

    let err = esi.character(1).await.unwrap_err();
    assert!(matches!(err, EsiError::Status(404)), "{err:?}");
    assert!(err.is_not_found());

    let ids: Vec<i64> = (1..=1500).collect();
    let names = esi.names(&ids).await.unwrap();
    assert_eq!(names.len(), 1500);
    assert_eq!(names[&1234], "Pilot 1234");
}

#[tokio::test]
async fn wars_round_trip_etags_and_report_missing_wars() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wars"))
        .and(header("if-none-match", "\"abc\""))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"abc\""))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/wars"))
        .and(query_param("max_war_id", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([499, 498])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/wars/7"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"w7\"")
                .set_body_json(json!({
                    "id": 7,
                    "aggressor": {"corporation_id": 1, "isk_destroyed": 10.0, "ships_killed": 1},
                    "defender": {"alliance_id": 2, "isk_destroyed": 0.0, "ships_killed": 0},
                    "allies": [{"alliance_id": 3}],
                    "declared": "2024-05-01T12:00:00Z",
                    "started": "2024-05-02T12:00:00Z",
                    "mutual": false,
                    "open_for_allies": true
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/wars/8"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "War not found"})))
        .mount(&server)
        .await;
    let esi = esi_without_db(&server);

    let unchanged = esi.wars(Some("abc")).await.unwrap();
    assert_eq!(unchanged.data, None);
    assert_eq!(unchanged.etag.as_deref(), Some("abc"));
    assert_eq!(esi.wars_page(500).await.unwrap(), [499, 498]);

    let war = esi.war(7, None).await.unwrap();
    assert_eq!(war.etag.as_deref(), Some("w7"));
    let war = war.data.unwrap();
    assert_eq!(war.declared.as_deref(), Some("2024-05-01T12:00:00Z"));
    assert_eq!(war.finished, None);
    assert_eq!(war.allies[0].alliance_id, Some(3));
    assert_eq!(war.aggressor.ships_killed, Some(1));

    let missing = esi.war(8, None).await.unwrap_err();
    assert!(
        matches!(&missing, EsiError::WithMessage(404, body) if body.contains("War not found")),
        "{missing:?}"
    );
}

/// Store tokens for a character as a login would (via the mock token endpoint).
async fn log_in(server: &MockServer, esi: &Esi, character_id: i64, scopes: &[&str], refresh: &str) {
    let guard = token_endpoint()
        .and(body_string_contains("grant_type=authorization_code"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_body(&jwt(character_id, "Pilot", scopes), refresh)),
        )
        .mount_as_scoped(server)
        .await;
    esi.process_authorization_code("code").await.unwrap();
    drop(guard);
}

async fn expire_access_token(db: &TestDb, character_id: i64) {
    sqlx::query("UPDATE access_token SET expires = 0 WHERE character_id = ?")
        .bind(character_id)
        .execute(db.pool())
        .await
        .unwrap();
}

async fn stored(db: &TestDb, character_id: i64) -> Option<(String, String)> {
    sqlx::query_as("SELECT refresh_token, scopes FROM refresh_token WHERE character_id = ?")
        .bind(character_id)
        .fetch_optional(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn login_stores_tokens_and_merges_earlier_scopes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    let esi = Esi::with_options(options(&server), db.pool().clone()).unwrap();

    // First login as FC.
    let fc_scopes: Vec<&str> = BASE_SCOPES.iter().chain(&FC_SCOPES).copied().collect();
    token_endpoint()
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=first"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_body(&jwt(42, "Fleet Boss", &fc_scopes), "r1")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let auth = esi.process_authorization_code("first").await.unwrap();
    assert_eq!(auth.character_id, 42);
    assert_eq!(auth.character_name, "Fleet Boss");
    let name: (String,) = sqlx::query_as("SELECT name FROM `character` WHERE id = 42")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(name.0, "Fleet Boss");
    let (refresh, scopes) = stored(&db, 42).await.unwrap();
    assert_eq!(refresh, "r1");
    assert_eq!(
        scopes,
        "esi-clones.read_implants.v1 esi-fleets.read_fleet.v1 esi-fleets.write_fleet.v1 \
         esi-skills.read_skills.v1 publicData "
    );

    // A later normal login asks for the union, so the FC scopes survive.
    token_endpoint()
        .and(body_string_contains("code=second"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_body(&jwt(42, "Fleet Boss", &BASE_SCOPES), "r2")),
        )
        .expect(1)
        .mount(&server)
        .await;
    token_endpoint()
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=r2"))
        .and(body_string_contains("esi-fleets.write_fleet.v1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_body(&jwt(42, "Fleet Boss", &fc_scopes), "r3")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let auth = esi.process_authorization_code("second").await.unwrap();
    assert!(auth.scopes.contains("esi-fleets.write_fleet.v1"));
    assert_eq!(stored(&db, 42).await.unwrap().0, "r3");

    // A rejected code is a 400 for the API.
    token_endpoint()
        .and(body_string_contains("code=stale"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"error": "invalid_grant", "error_description": "Authorization code not found"}),
        ))
        .mount(&server)
        .await;
    let err = esi.process_authorization_code("stale").await.unwrap_err();
    assert!(matches!(err, EsiError::WithMessage(400, _)), "{err:?}");
}

#[tokio::test]
async fn expired_tokens_refresh_once_and_store_the_rotated_refresh_token() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    let esi = Esi::with_options(options(&server), db.pool().clone()).unwrap();
    log_in(&server, &esi, 7, &BASE_SCOPES, "old-refresh").await;

    // A fresh token is used as stored.
    let token = esi.access_token(7, Scope::SkillsReadSkills).await.unwrap();
    assert_eq!(token, jwt(7, "Pilot", &BASE_SCOPES));

    expire_access_token(&db, 7).await;
    let refreshed = jwt(7, "Pilot", &BASE_SCOPES).replace(".sig", ".sig2");
    struct Slow(String);
    impl Respond for Slow {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_millis(100))
                .set_body_json(token_body(&self.0, "new-refresh"))
        }
    }
    token_endpoint()
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=old-refresh"))
        .and(body_string_contains("scope=esi-clones.read_implants.v1"))
        .respond_with(Slow(refreshed.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let calls: Vec<_> = (0..5)
        .map(|_| {
            let esi = esi.clone();
            tokio::spawn(async move { esi.access_token(7, Scope::PublicData).await })
        })
        .collect();
    for call in calls {
        assert_eq!(call.await.unwrap().unwrap(), refreshed);
    }
    assert_eq!(stored(&db, 7).await.unwrap().0, "new-refresh");

    assert!(matches!(
        esi.access_token(7, Scope::FleetsWriteFleet).await,
        Err(EsiError::MissingScope)
    ));
    assert!(matches!(
        esi.access_token(8, Scope::PublicData).await,
        Err(EsiError::NoToken)
    ));
}

#[tokio::test]
async fn revoked_tokens_are_deleted_but_transient_failures_keep_them() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    let esi = Esi::with_options(options(&server), db.pool().clone()).unwrap();
    log_in(&server, &esi, 1, &BASE_SCOPES, "flaky").await;
    log_in(&server, &esi, 2, &BASE_SCOPES, "revoked").await;
    log_in(&server, &esi, 3, &BASE_SCOPES, "bad-client").await;
    for id in [1, 2, 3] {
        expire_access_token(&db, id).await;
    }
    token_endpoint()
        .and(body_string_contains("refresh_token=flaky"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>Bad gateway</html>"))
        .mount(&server)
        .await;
    token_endpoint()
        .and(body_string_contains("refresh_token=revoked"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant", "error_description": "Invalid refresh token. Token missing/expired."})))
        .mount(&server)
        .await;
    token_endpoint()
        .and(body_string_contains("refresh_token=bad-client"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_client"})))
        .mount(&server)
        .await;

    let err = esi.access_token(1, Scope::PublicData).await.unwrap_err();
    assert!(matches!(err, EsiError::Http(_)), "{err:?}");
    assert!(stored(&db, 1).await.is_some());

    let err = esi.access_token(2, Scope::PublicData).await.unwrap_err();
    assert!(matches!(err, EsiError::NoToken), "{err:?}");
    assert!(stored(&db, 2).await.is_none());
    let access: Option<(i64,)> =
        sqlx::query_as("SELECT character_id FROM access_token WHERE character_id = 2")
            .fetch_optional(db.pool())
            .await
            .unwrap();
    assert!(access.is_none());

    // A client-level rejection (wrong secret) must not wipe the token.
    let err = esi.access_token(3, Scope::PublicData).await.unwrap_err();
    assert!(matches!(err, EsiError::Http(_)), "{err:?}");
    assert!(stored(&db, 3).await.is_some());
}

#[tokio::test]
async fn authenticated_calls_use_the_characters_token_and_legacy_error_styles() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    let esi = Esi::with_options(options(&server), db.pool().clone()).unwrap();
    let fc_scopes: Vec<&str> = BASE_SCOPES.iter().chain(&FC_SCOPES).copied().collect();
    log_in(&server, &esi, 100, &fc_scopes, "r").await;
    log_in(&server, &esi, 200, &BASE_SCOPES, "r").await;
    let bearer = format!("Bearer {}", jwt(100, "Pilot", &fc_scopes));

    Mock::given(method("GET"))
        .and(path("/characters/100/fleet"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "fleet_id": 1234, "fleet_boss_id": 100, "role": "fleet_commander",
            "squad_id": -1, "wing_id": -1
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/fleets/1234/members"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "character_id": 300, "join_time": "2024-05-01T12:00:00Z", "role": "squad_member",
            "role_name": "Squad Member (Squad 1)", "ship_type_id": 17738, "solar_system_id": 30000142,
            "squad_id": 5, "takes_fleet_warp": true, "wing_id": 4
        }])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/fleets/1234/members"))
        .and(body_string_contains("\"role\":\"squad_member\""))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "error": "Character is already in a fleet, character_id: 300"
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/fleets/1234/members/301"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "Not found"})))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/fleets/1234"))
        .and(body_string_contains("\"motd\":\"hello\""))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/characters/200/fleet"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": "Character is not in a fleet"})),
        )
        .mount(&server)
        .await;
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    Mock::given(method("GET"))
        .and(path("/characters/200/skills"))
        .respond_with(move |_: &Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "skills": [{"skill_id": 3336, "active_skill_level": 4, "trained_skill_level": 5, "skillpoints_in_skill": 256000}],
                "total_sp": 256000
            }))
        })
        .mount(&server)
        .await;

    let fleet = esi.current_fleet(100).await.unwrap();
    assert_eq!(
        (fleet.fleet_id, fleet.role.as_str()),
        (1234, "fleet_commander")
    );
    let members = esi.fleet_members(100, 1234).await.unwrap();
    assert_eq!(members[0].ship_type_id, 17738);
    assert_eq!(members[0].role, "squad_member");
    assert_eq!(members[0].join_time, "2024-05-01T12:00:00Z");

    // POST failures keep ESI's status and the reason before ", ".
    let err = esi.invite_to_fleet(100, 1234, 300, 4, 5).await.unwrap_err();
    assert!(
        matches!(&err, EsiError::WithMessage(422, m) if m == "Character is already in a fleet"),
        "{err:?}"
    );
    // DELETE failures are bare statuses.
    let err = esi.kick_fleet_member(100, 1234, 301).await.unwrap_err();
    assert!(matches!(err, EsiError::Status(404)), "{err:?}");
    esi.update_fleet_motd(100, 1234, "hello".into())
        .await
        .unwrap();

    // Authenticated GET failures carry ESI's status and body; a missing scope never calls ESI.
    let err = esi.current_fleet(200).await.unwrap_err();
    assert!(matches!(err, EsiError::MissingScope), "{err:?}");
    let skills = esi.skills(200).await.unwrap();
    assert_eq!(
        (
            skills[0].skill_id,
            skills[0].trained_skill_level,
            skills[0].active_skill_level
        ),
        (3336, 5, 4)
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Give 200 the fleet scope to see the 404 body.
    sqlx::query("UPDATE access_token SET scopes = CONCAT(scopes, 'esi-fleets.read_fleet.v1 ') WHERE character_id = 200")
        .execute(db.pool())
        .await
        .unwrap();
    let err = esi.current_fleet(200).await.unwrap_err();
    assert!(err.is_not_found(), "{err:?}");
    assert!(
        matches!(&err, EsiError::WithMessage(404, body) if body.contains("Character is not in a fleet")),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        r#"{"error":"Character is not in a fleet"}"#
    );
}
