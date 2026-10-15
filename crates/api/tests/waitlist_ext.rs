//! Extensions beyond the legacy waitlist API: waitlist administration, gate data, recorded
//! invites, closed lists for FCs and the public status (with its stream). Also checks that the
//! legacy contract around them is unchanged.

mod common;
mod support;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{Login, TestApp};
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::{Env, next_event, no_event};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};
use wl_live::{events, topics};

/// X-up `fit` (a doctrine fit) as the login's main on waitlist `waitlist_id`.
async fn xup(env: &Env, login: &Login, waitlist_id: i64, fit: &str) {
    let character_id = login.account_id;
    let res = env
        .post_json(
            "/api/waitlist/xup",
            Some(login),
            &json!({
                "waitlist_id": waitlist_id, "character_id": character_id, "eft": "",
                "is_alt": false, "messagexup": "hi",
                "dna": [{"character_id": character_id, "dna": env.dna(fit)}],
            }),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

async fn get_json(app: &TestApp, uri: &str, login: Option<&Login>) -> Value {
    let res = app.get(uri, login).await;
    assert_eq!(res.status, StatusCode::OK, "{uri}: {}", res.text());
    res.json()
}

async fn create(app: &TestApp, admin: &Login, name: &str) -> i64 {
    let res = app
        .post_json("/api/admin/waitlists", Some(admin), &json!({"name": name}))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json::<Value>()["id"].as_i64().unwrap()
}

async fn set_open(app: &TestApp, fc: &Login, waitlist_id: i64, open: bool) -> (StatusCode, String) {
    let res = app
        .post_json(
            "/api/waitlist/set_open",
            Some(fc),
            &json!({"open": open, "waitlist_id": waitlist_id}),
        )
        .await;
    (res.status, res.text().to_string())
}

fn not_found() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "Waitlist not found".to_string())
}

#[tokio::test]
async fn waitlists_are_listed_created_renamed_and_archived() {
    let Some(app) = Env::new().await else { return };
    let user = app.create_account(1, "User", None).await;
    let trainee = app.create_account(2, "Trainee", Some("trainee")).await;
    let fc = app.create_account(3, "Fc", Some("fc+")).await;
    let admin = app.create_account(4, "Admin", Some("admin")).await;

    // Reads need waitlist-view; changes need commanders-manage:admin.
    assert_eq!(app.get("/api/waitlists", None).await.status, 401);
    let res = app.get("/api/waitlists", Some(&user)).await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    assert_eq!(
        get_json(&app, "/api/waitlists", Some(&trainee)).await,
        json!({"waitlists": [{"id": 1, "name": "Main", "open": false, "archived": false, "waiting": 0}]})
    );
    let res = app
        .post_json(
            "/api/admin/waitlists",
            Some(&fc),
            &json!({"name": "Vanguard"}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = app
        .send_json(
            Method::PATCH,
            "/api/admin/waitlists/1",
            Some(&fc),
            &json!({"name": "HQ"}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );

    // Create: closed, announced on both topics.
    let mut sub = app
        .live()
        .subscribe(vec![topics::WAITLIST.into(), topics::STATUS.into()]);
    let res = app
        .post_json(
            "/api/admin/waitlists",
            Some(&admin),
            &json!({"name": "  Vanguard "}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let created: Value = res.json();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(
        created,
        json!({"id": id, "name": "Vanguard", "open": false, "archived": false, "waiting": 0})
    );
    let data = format!(r#"{{"waitlist_id":{id}}}"#);
    let event = next_event(&mut sub).await;
    assert_eq!(
        (
            event.topic.as_str(),
            event.name.as_str(),
            event.data.as_str()
        ),
        (topics::WAITLIST, events::WAITLIST_UPDATE, data.as_str())
    );
    let event = next_event(&mut sub).await;
    assert_eq!(
        (
            event.topic.as_str(),
            event.name.as_str(),
            event.data.as_str()
        ),
        (topics::STATUS, events::STATUS_UPDATE, data.as_str())
    );
    for name in ["", "   ", &"x".repeat(256)] {
        let res = app
            .post_json("/api/admin/waitlists", Some(&admin), &json!({"name": name}))
            .await;
        assert_eq!(
            (res.status, res.text()),
            (StatusCode::BAD_REQUEST, "Invalid waitlist name")
        );
    }

    // The new waitlist works with the legacy endpoints.
    assert_eq!(
        set_open(&app, &fc, id, true).await,
        (StatusCode::OK, "OK".into())
    );
    let view = get_json(
        &app,
        &format!("/api/waitlist?waitlist_id={id}"),
        Some(&user),
    )
    .await;
    assert_eq!(view["open"], json!(true));
    assert_eq!(view["waitlist"], json!([]));

    // Rename.
    let path = format!("/api/admin/waitlists/{id}");
    let res = app
        .send_json(
            Method::PATCH,
            &path,
            Some(&admin),
            &json!({"name": "Assault"}),
        )
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"id": id, "name": "Assault", "open": true, "archived": false, "waiting": 0})
    );
    let res = app
        .send_json(Method::PATCH, &path, Some(&admin), &json!({"name": ""}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = app
        .send_json(
            Method::PATCH,
            "/api/admin/waitlists/99",
            Some(&admin),
            &json!({"name": "X"}),
        )
        .await;
    assert_eq!((res.status, res.text().to_string()), not_found());

    // Archive: closed, hidden from the gate and status views, set_open/empty answer 404.
    let res = app
        .send_json(
            Method::PATCH,
            &path,
            Some(&admin),
            &json!({"archived": true}),
        )
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"id": id, "name": "Assault", "open": false, "archived": true, "waiting": 0})
    );
    assert_eq!(set_open(&app, &fc, id, true).await, not_found());
    let res = app
        .post_json(
            "/api/waitlist/empty",
            Some(&fc),
            &json!({"waitlist_id": id}),
        )
        .await;
    assert_eq!((res.status, res.text().to_string()), not_found());
    let status = get_json(&app, "/api/public/status", None).await;
    assert_eq!(status["waitlists"].as_array().unwrap().len(), 1, "{status}");
    let all = get_json(&app, "/api/waitlists", Some(&trainee)).await;
    assert_eq!(all["waitlists"][1]["archived"], json!(true));

    // Restore: back, closed.
    let res = app
        .send_json(
            Method::PATCH,
            &path,
            Some(&admin),
            &json!({"archived": false}),
        )
        .await;
    assert_eq!(res.json::<Value>()["archived"], json!(false));
    assert_eq!(
        set_open(&app, &fc, id, false).await,
        (StatusCode::OK, "OK".into())
    );
}

#[tokio::test]
async fn set_open_and_empty_answer_404_for_unknown_waitlists() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1, "Fc", Some("fc")).await;
    let user = app.create_account(2, "User", None).await;
    let mut sub = app.live().subscribe(vec![topics::WAITLIST.into()]);
    assert_eq!(set_open(&app, &fc, 42, true).await, not_found());
    let res = app
        .post_json(
            "/api/waitlist/empty",
            Some(&fc),
            &json!({"waitlist_id": 42}),
        )
        .await;
    assert_eq!((res.status, res.text().to_string()), not_found());
    no_event(&mut sub).await;
    // The permission is still checked first.
    let res = app
        .post_json(
            "/api/waitlist/set_open",
            Some(&user),
            &json!({"open": true, "waitlist_id": 42}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
}

#[tokio::test]
async fn archiving_removes_entries_and_fleet_links() {
    let Some(env) = Env::new().await else { return };
    let admin = env.pilot(9001, Some("admin")).await;
    let pilot = env.pilot(1001, None).await;
    let id = create(&env, &admin, "Vanguard").await;
    set_open(&env, &admin, id, true).await;
    xup(&env, &pilot, id, "ARMOR_PALADIN").await;
    sqlx::query("INSERT INTO fleet (id, boss_id) VALUES (77, 9001)")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO fleet_waitlist (fleet_id, waitlist_id) VALUES (77, ?)")
        .bind(id)
        .execute(env.pool())
        .await
        .unwrap();

    let res = env
        .send_json(
            Method::PATCH,
            &format!("/api/admin/waitlists/{id}"),
            Some(&admin),
            &json!({"archived": true}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let left: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM waitlist_entry), (SELECT COUNT(*) FROM waitlist_entry_fit),
            (SELECT COUNT(*) FROM fleet_waitlist)",
    )
    .fetch_one(env.pool())
    .await
    .unwrap();
    assert_eq!(left, (0, 0, 0));
    let fleets = get_json(&env, "/api/fleet/status", Some(&admin)).await;
    assert_eq!(fleets["fleets"][0]["waitlist_id"], Value::Null);
}

#[tokio::test]
async fn gates_show_counts_and_linked_fcs_without_pilots() {
    let Some(env) = Env::new().await else { return };
    let admin = env.pilot(9001, Some("admin")).await;
    let fc = env.pilot(2001, Some("trainee")).await;
    let pilot = env.pilot(1001, None).await;
    let other = env.pilot(1002, None).await;
    let vanguard = create(&env, &admin, "Vanguard").await;
    env.open_waitlist().await;
    xup(&env, &pilot, 1, "ARMOR_PALADIN").await;
    xup(&env, &other, 1, "ARMOR_PALADIN").await;

    assert_eq!(env.get("/api/waitlist/gates", None).await.status, 401);
    let categories = |bastion: i64| {
        json!([
            {"id": "logi", "name": "Logistics", "count": 0},
            {"id": "bastion", "name": "Bastion", "count": bastion},
            {"id": "other", "name": "Other", "count": 0},
        ])
    };
    let gates = get_json(&env, "/api/waitlist/gates", Some(&pilot)).await;
    assert_eq!(
        gates,
        json!({"gates": [
            {"id": 1, "name": "Main", "open": true, "waiting": 2, "fcs": [], "queued": categories(2)},
            {"id": vanguard, "name": "Vanguard", "open": false, "waiting": 0, "fcs": [], "queued": categories(0)},
        ]})
    );
    assert!(!gates.to_string().contains("Pilot 100"), "no pilot names");

    // Registering a fleet for a waitlist names its boss on that gate and announces it.
    let assignments = json!({"logi": [1, 11], "bastion": [1, 12], "other": [1, 12]});
    let res = env
        .post_json(
            "/api/fleet/register",
            Some(&fc),
            &json!({"character_id": 2001, "fleet_id": 77, "assignments": assignments, "waitlist_id": 99}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::NOT_FOUND, "Waitlist not found")
    );
    assert_eq!(
        get_json(&env, "/api/fleet/status", Some(&fc)).await["fleets"],
        json!([])
    );

    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);
    let register = |waitlist_id: Option<i64>| {
        let mut body = json!({"character_id": 2001, "fleet_id": 77, "assignments": assignments});
        if let Some(id) = waitlist_id {
            body["waitlist_id"] = json!(id);
        }
        body
    };
    let res = env
        .post_json("/api/fleet/register", Some(&fc), &register(Some(vanguard)))
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    let event = next_event(&mut sub).await;
    assert_eq!(event.data, format!(r#"{{"waitlist_id":{vanguard}}}"#));
    let gates = get_json(&env, "/api/waitlist/gates", Some(&pilot)).await;
    assert_eq!(gates["gates"][0]["fcs"], json!([]));
    assert_eq!(
        gates["gates"][1]["fcs"],
        json!([{"id": 2001, "name": "Pilot 2001"}])
    );
    assert_eq!(
        get_json(&env, "/api/fleet/status", Some(&fc)).await["fleets"],
        json!([{"id": 77, "boss": {"id": 2001, "name": "Pilot 2001"}, "waitlist_id": vanguard}])
    );

    // Re-registering without a waitlist (as a legacy frontend does) removes the link.
    let res = env
        .post_json("/api/fleet/register", Some(&fc), &register(None))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let event = next_event(&mut sub).await;
    assert_eq!(event.data, format!(r#"{{"waitlist_id":{vanguard}}}"#));
    no_event(&mut sub).await;
    let gates = get_json(&env, "/api/waitlist/gates", Some(&pilot)).await;
    assert_eq!(gates["gates"][1]["fcs"], json!([]));
}

#[tokio::test]
async fn invites_are_recorded_shown_and_cleared() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let stranger = env.pilot(1002, None).await;
    let fc = env.pilot(2001, Some("trainee")).await;
    xup(&env, &pilot, 1, "ARMOR_PALADIN").await;
    let fit: i64 = sqlx::query_scalar("SELECT id FROM waitlist_entry_fit")
        .fetch_one(env.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO fleet (id, boss_id) VALUES (77, 2001)")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO fleet_squad (fleet_id, category, wing_id, squad_id) VALUES (77, 'bastion', 10, 11)")
        .execute(env.pool())
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/fleets/77/members"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&env.esi)
        .await;

    let invited = |view: Value| view["waitlist"][0]["fits"][0]["invited"].clone();
    let list = "/api/waitlist?waitlist_id=1";
    assert_eq!(invited(get_json(&env, list, Some(&fc)).await), Value::Null);

    let before = chrono::Utc::now().timestamp();
    let res = env
        .post_json(
            "/api/waitlist/invite",
            Some(&fc),
            &json!({"id": fit, "character_id": 2001}),
        )
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    let seen = invited(get_json(&env, list, Some(&fc)).await);
    assert_eq!(seen["by"], json!({"id": 2001, "name": "Pilot 2001"}));
    let at = seen["at"].as_i64().unwrap();
    assert!((before..=before + 5).contains(&at), "{at}");
    // The pilot sees their own invite; other pilots don't.
    assert_eq!(invited(get_json(&env, list, Some(&pilot)).await), seen);
    assert_eq!(
        invited(get_json(&env, list, Some(&stranger)).await),
        Value::Null
    );

    // Clearing ("undo") needs fleet-invite, forgets the record and announces it.
    let res = env
        .post_json(
            "/api/waitlist/invite/clear",
            Some(&pilot),
            &json!({"id": fit}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = env
        .post_json(
            "/api/waitlist/invite/clear",
            Some(&fc),
            &json!({"id": 999_999}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::NOT_FOUND, "Fit not found")
    );
    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);
    let res = env
        .post_json("/api/waitlist/invite/clear", Some(&fc), &json!({"id": fit}))
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    assert_eq!(next_event(&mut sub).await.data, r#"{"waitlist_id":1}"#);
    assert_eq!(invited(get_json(&env, list, Some(&fc)).await), Value::Null);

    // The record goes away with the fit.
    let res = env
        .post_json(
            "/api/waitlist/invite",
            Some(&fc),
            &json!({"id": fit, "character_id": 2001}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let res = env
        .post_json(
            "/api/waitlist/remove_fit",
            Some(&pilot),
            &json!({"id": fit}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let records: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waitlist_invite")
        .fetch_one(env.pool())
        .await
        .unwrap();
    assert_eq!(records, 0);
}

#[tokio::test]
async fn closed_lists_are_shown_to_fcs_only_on_request() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let fc = env.pilot(2001, Some("trainee")).await;
    xup(&env, &pilot, 1, "ARMOR_PALADIN").await;
    sqlx::query("UPDATE waitlist SET is_open = 0")
        .execute(env.pool())
        .await
        .unwrap();
    let closed =
        json!({"open": false, "waitlist": null, "categories": ["Logistics", "Bastion", "Other"]});

    // Default unchanged: a closed list has no entries, for everyone.
    assert_eq!(
        get_json(&env, "/api/waitlist?waitlist_id=1", Some(&fc)).await,
        closed
    );
    // include_closed: entries for waitlist-view only.
    let shown = get_json(
        &env,
        "/api/waitlist?waitlist_id=1&include_closed=true",
        Some(&fc),
    )
    .await;
    assert_eq!(shown["open"], json!(false));
    assert_eq!(shown["waitlist"][0]["character"]["id"], json!(1001));
    assert_eq!(
        get_json(
            &env,
            "/api/waitlist?waitlist_id=1&include_closed=true",
            Some(&pilot)
        )
        .await,
        closed
    );
    assert_eq!(
        get_json(
            &env,
            "/api/waitlist?waitlist_id=1&include_closed=false",
            Some(&fc)
        )
        .await,
        closed
    );
    assert_eq!(
        get_json(
            &env,
            "/api/waitlist?waitlist_id=7&include_closed=true",
            Some(&fc)
        )
        .await,
        closed
    );
}

/// The legacy fields of a waitlist fit are unchanged; `invited` is the only addition.
#[tokio::test]
async fn legacy_waitlist_fields_are_unchanged() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let stranger = env.pilot(1002, None).await;
    xup(&env, &pilot, 1, "ARMOR_PALADIN").await;
    let view = get_json(&env, "/api/waitlist?waitlist_id=1", Some(&stranger)).await;
    let keys = |v: &Value| {
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(keys(&view), ["categories", "open", "waitlist"]);
    assert_eq!(
        keys(&view["waitlist"][0]),
        [
            "can_remove",
            "character",
            "fits",
            "id",
            "joined_at",
            "waitlist_note"
        ]
    );
    assert_eq!(
        keys(&view["waitlist"][0]["fits"][0]),
        [
            "approved",
            "category",
            "character",
            "dna",
            "fit_analysis",
            "hours_in_fleet",
            "hull",
            "id",
            "implants",
            "invited",
            "is_alt",
            "messagexup",
            "review_comment",
            "tags",
        ]
    );
}

#[tokio::test]
async fn public_status_and_stream() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1, "Fc", Some("fc")).await;

    let res = app.get("/api/public/status", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.header("cache-control"), Some("public, no-cache"));
    let etag = res.header("etag").unwrap().to_owned();
    assert_eq!(
        res.json::<Value>(),
        json!({"waitlists": [{"id": 1, "name": "Main", "open": false, "waiting": 0}]})
    );
    let if_none_match = |etag: String| {
        let request = TestApp::build(Method::GET, "/api/public/status", None)
            .header("if-none-match", etag)
            .body(axum::body::Body::empty())
            .unwrap();
        app.request(request)
    };
    let res = if_none_match(etag.clone()).await;
    assert_eq!(res.status, StatusCode::NOT_MODIFIED);
    assert!(res.body.is_empty());

    let mut public = open_stream(&app, "/api/public/stream", None).await;

    // Opening the waitlist reaches anonymous clients as `status_update`. Incursion updates
    // arrive too.
    assert_eq!(
        set_open(&app, &fc, 1, true).await,
        (StatusCode::OK, "OK".into())
    );
    assert_eq!(
        next_chunk(&mut public).await,
        "event: status_update\ndata: {\"waitlist_id\":1}\n\n"
    );
    app.live().publish(
        topics::INCURSIONS,
        events::INCURSIONS_UPDATE,
        r#"{"spawn_ids":[1]}"#,
    );
    assert_eq!(
        next_chunk(&mut public).await,
        "event: incursions_update\ndata: {\"spawn_ids\":[1]}\n\n"
    );
    // The refetch after the event revalidates: the old tag no longer matches.
    let res = if_none_match(etag.clone()).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_ne!(res.header("etag"), Some(etag.as_str()));
    assert_eq!(res.json::<Value>()["waitlists"][0]["open"], json!(true));

    // The incursion stream still carries only incursions, the logged-in stream no status.
    let user = app.create_account(2, "User", None).await;
    let mut incursions = open_stream(&app, "/api/incursions/stream", None).await;
    let mut logged_in = open_stream(&app, "/api/sse/stream", Some(&user)).await;
    app.live().waitlist_changed(1);
    app.live().publish(
        topics::INCURSIONS,
        events::INCURSIONS_UPDATE,
        r#"{"spawn_ids":[2]}"#,
    );
    assert_eq!(
        next_chunk(&mut incursions).await,
        "event: incursions_update\ndata: {\"spawn_ids\":[2]}\n\n"
    );
    assert_eq!(
        next_chunk(&mut logged_in).await,
        "event: waitlist_update\ndata: {\"waitlist_id\":1}\n\n"
    );
    app.live()
        .publish(topics::ANNOUNCEMENTS, events::ANNOUNCEMENT_NEW, "[]");
    assert_eq!(
        next_chunk(&mut logged_in).await,
        "event: announcment;new\ndata: []\n\n",
        "no status_update in between"
    );
}

async fn next_chunk(body: &mut axum::body::BodyDataStream) -> String {
    let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("an event")
        .expect("stream open")
        .unwrap();
    String::from_utf8(chunk.to_vec()).unwrap()
}

/// Open an event stream and wait until it is subscribed.
async fn open_stream(
    app: &TestApp,
    uri: &str,
    login: Option<&Login>,
) -> axum::body::BodyDataStream {
    let subscribers = app.live().subscriber_count();
    let request = TestApp::build(Method::GET, uri, login)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body().into_data_stream();
    assert_eq!(next_chunk(&mut body).await, ": connected\n\n");
    for _ in 0..100 {
        if app.live().subscriber_count() > subscribers {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{uri}: not subscribed");
}
