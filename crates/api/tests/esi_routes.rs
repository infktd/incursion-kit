//! Routes that call ESI: bans (entity name lookup) and `POST /api/search`, against a mock ESI.

mod common;

use axum::http::Method;
use common::TestApp;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_esi::{Esi, EsiOptions};

async fn app_with_esi(server: &MockServer) -> Option<TestApp> {
    let uri = server.uri();
    TestApp::with_services(|state, db, _| {
        let esi = Esi::with_options(
            EsiOptions {
                client_id: "test-client".into(),
                client_secret: "test-secret".into(),
                redirect_url: "http://waitlist.test/auth/cb".into(),
                user_agent: "wl-api tests".into(),
                esi_url: uri.clone(),
                sso_authorize_url: format!("{uri}/v2/oauth/authorize"),
                sso_token_url: format!("{uri}/v2/oauth/token"),
            },
            db.clone(),
        )
        .expect("ESI client");
        state.with_esi(esi)
    })
    .await
}

fn character_detail(name: &str) -> Value {
    json!({
        "achievement_score": 0,
        "birthday": "2015-03-24T11:37:00Z",
        "bloodline_id": 3,
        "corporation_id": 98000001,
        "gender": "male",
        "name": name,
        "race_id": 2,
    })
}

const DAY: i64 = 1_900_000_000 - 1_900_000_000 % 86400;

// --- bans ------------------------------------------------------------------------------------

#[tokio::test]
async fn bans_create_list_history_update_revoke() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"/characters/2001/?$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(character_detail("Bad Pilot")))
        .mount(&server)
        .await;
    let Some(app) = app_with_esi(&server).await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    let other_fc = app.create_account(1002, "Other Fc", Some("fc")).await;

    // The client-sent name is ignored; the expiry day gets the downtime offset.
    let res = app
        .post_json(
            "/api/v2/bans",
            Some(&fc),
            &json!({
                "entity": {"id": 2001, "category": "Character", "name": "whatever"},
                "reason": "internal",
                "public_reason": "public",
                "revoked_at": DAY,
            }),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));

    let bans: Value = app.get("/api/v2/bans", Some(&fc)).await.json();
    let id = bans[0]["id"].as_i64().unwrap();
    assert_eq!(
        bans,
        json!([{
            "id": id,
            "entity": {"id": 2001, "name": "Bad Pilot", "category": "Character"},
            "issued_at": bans[0]["issued_at"],
            "issued_by": {"id": 1001, "name": "Fc"},
            "public_reason": "public",
            "reason": "internal",
            "revoked_at": DAY + 11 * 3600,
            "revoked_by": null,
        }])
    );

    // Editing re-attributes the ban.
    let res = app
        .send_json(
            Method::PATCH,
            &format!("/api/v2/bans/{id}"),
            Some(&other_fc),
            &json!({"reason": "edited", "public_reason": null, "revoked_at": null}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    let bans: Value = app.get("/api/v2/bans", Some(&fc)).await.json();
    assert_eq!(bans[0]["reason"], "edited");
    assert_eq!(bans[0]["revoked_at"], Value::Null);
    assert_eq!(bans[0]["issued_by"]["id"], 1002);

    let res = app.delete(&format!("/api/v2/bans/{id}"), Some(&fc)).await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    assert_eq!(
        app.get("/api/v2/bans", Some(&fc)).await.json::<Value>(),
        json!([])
    );

    let history: Value = app.get("/api/v2/bans/2001", Some(&fc)).await.json();
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["revoked_by"], json!({"id": 1001, "name": "Fc"}));
    assert_eq!(
        app.get("/api/v2/bans/9999", Some(&fc))
            .await
            .json::<Value>(),
        json!([])
    );

    // A revoked ban can't be edited or revoked again (once its revocation is in the past).
    sqlx::query("UPDATE ban SET revoked_at = revoked_at - 10 WHERE id = ?")
        .bind(id)
        .execute(app.pool())
        .await
        .unwrap();
    let res = app
        .send_json(
            Method::PATCH,
            &format!("/api/v2/bans/{id}"),
            Some(&fc),
            &json!({"reason": "x"}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Cannot revoke invalid ban. It is either invalid or doesn't exist"
        )
    );
    let res = app.delete(&format!("/api/v2/bans/{id}"), Some(&fc)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Fc has already revoked this ban")
    );
    let res = app.delete("/api/v2/bans/9999", Some(&fc)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Could not find a ban with the ID of 9999")
    );
}

#[tokio::test]
async fn bans_validation_and_expiry() {
    let server = MockServer::start().await;
    let Some(app) = app_with_esi(&server).await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;

    let res = app
        .post_json("/api/v2/bans", Some(&fc), &json!({"reason": "r"}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "One or more body paramaters are missing: [\"id\", \"name\", \"kind\"]"
        )
    );
    let res = app
        .post_json(
            "/api/v2/bans",
            Some(&fc),
            &json!({"entity": {"id": 1, "category": "Character"}}),
        )
        .await;
    assert_eq!(res.status, 422, "reason is required");
    // ESI has no endpoint for accounts: the legacy outcome was a 500 from the failed lookup.
    let res = app
        .post_json(
            "/api/v2/bans",
            Some(&fc),
            &json!({"entity": {"id": 1001, "category": "Account"}, "reason": "r"}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (500, "ESI error: ESI returned 404")
    );

    // An expired ban (no revoker) can't be revoked.
    sqlx::query(
        "INSERT INTO ban (id, entity_type, entity_id, entity_name, issued_at, issued_by, reason, revoked_at)
         VALUES (77, 'Character', 1001, 'Fc', 0, 1001, 'r', 10)",
    )
    .execute(app.pool())
    .await
    .unwrap();
    let res = app.delete("/api/v2/bans/77", Some(&fc)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Cannot revoke the ban as it has already expired")
    );
    assert_eq!(
        app.get("/api/v2/bans", Some(&fc)).await.json::<Value>(),
        json!([])
    );
}

#[tokio::test]
async fn bans_need_bans_manage() {
    let server = MockServer::start().await;
    let Some(app) = app_with_esi(&server).await else {
        return;
    };
    let trainee = app
        .create_account(1001, "T", Some("trainee-advanced"))
        .await;
    let body = json!({"entity": {"id": 1, "category": "Character"}, "reason": "r"});
    for res in [
        app.get("/api/v2/bans", Some(&trainee)).await,
        app.post_json("/api/v2/bans", Some(&trainee), &body).await,
        app.get("/api/v2/bans/1", Some(&trainee)).await,
        app.send_json(Method::PATCH, "/api/v2/bans/1", Some(&trainee), &body)
            .await,
        app.delete("/api/v2/bans/1", Some(&trainee)).await,
    ] {
        assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    }
    // PUT was never a ban route.
    let res = app
        .send_json(Method::PUT, "/api/v2/bans/1", Some(&trainee), &body)
        .await;
    assert_eq!(res.status, 404);
}

// --- ESI search ------------------------------------------------------------------------------

async fn store_token(app: &TestApp, character_id: i64, scopes: &str) {
    sqlx::query(
        "INSERT INTO access_token (character_id, access_token, expires, scopes) VALUES (?, 'token', ?, ?)",
    )
    .bind(character_id)
    .bind(chrono::Utc::now().timestamp() + 3600)
    .bind(scopes)
    .execute(app.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO refresh_token (character_id, refresh_token, scopes) VALUES (?, 'r', ?)",
    )
    .bind(character_id)
    .bind(scopes)
    .execute(app.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn esi_search_returns_first_category_with_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"/characters/1001/search/?$"))
        .and(query_param("search", "Some Name"))
        .and(query_param("strict", "true"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"corporation": [98000001, 98000002]})),
        )
        .mount(&server)
        .await;
    let Some(app) = app_with_esi(&server).await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("trainee")).await;
    store_token(&app, 1001, "publicData esi-search.search_structures.v1").await;

    let res = app
        .post_json(
            "/api/search",
            Some(&fc),
            &json!({"search": "Some Name", "category": "character,corporation", "strict": true}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json::<Value>(), json!([98000001, 98000002]));

    let res = app
        .post_json(
            "/api/search",
            Some(&fc),
            &json!({"search": "Some Name", "category": "station"}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Body paramater \"category\" must be one of [\"character\", \"corporation\", \"alliance\"]"
        )
    );
}

#[tokio::test]
async fn esi_search_needs_scope_and_permission() {
    let server = MockServer::start().await;
    let Some(app) = app_with_esi(&server).await else {
        return;
    };
    let user = app.create_account(1001, "User", None).await;
    let body = json!({"search": "Some Name", "category": "character"});
    let res = app.post_json("/api/search", Some(&user), &body).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));

    let fc = app.create_account(1002, "Fc", Some("fc")).await;
    let res = app.post_json("/api/search", Some(&fc), &body).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (401, "ESI error: no ESI token found")
    );
    store_token(&app, 1002, "publicData").await;
    let res = app.post_json("/api/search", Some(&fc), &body).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (401, "ESI error: missing ESI scope")
    );
}
