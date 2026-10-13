//! Announcements, badges, commanders, notes and the local character search.

mod common;

use std::time::Duration;

use axum::http::Method;
use common::TestApp;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wl_live::{Subscription, events, topics};

async fn next_event(sub: &mut Subscription) -> (String, Value) {
    let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timed out waiting for a live event")
        .expect("subscription ended");
    (
        event.name.clone(),
        serde_json::from_str(&event.data).expect("JSON event data"),
    )
}

async fn assert_no_event(sub: &mut Subscription) {
    let next = tokio::time::timeout(Duration::from_millis(100), sub.next()).await;
    assert!(next.is_err(), "unexpected live event: {next:?}");
}

// --- announcements ---------------------------------------------------------------------------

#[tokio::test]
async fn announcements_lifecycle_publishes_full_list() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1001, "Fc Main", Some("fc")).await;
    let other_fc = app.create_account(1002, "Other Fc", Some("fc")).await;
    let mut sub = app
        .live()
        .subscribe(vec![topics::ANNOUNCEMENTS.to_string()]);

    let res = app.get("/api/v2/announcements", None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.json::<Value>(), json!([]));

    let res = app
        .post_json(
            "/api/v2/announcements",
            Some(&fc),
            &json!({"message": "Fleet up!", "is_alert": true, "pages": null}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    let (name, data) = next_event(&mut sub).await;
    assert_eq!(name, events::ANNOUNCEMENT_NEW);
    let id = data[0]["id"].as_i64().unwrap();
    assert_eq!(
        data,
        json!([{
            "id": id,
            "message": "Fleet up!",
            "is_alert": true,
            "created_by": {"id": 1001, "name": "Fc Main"},
            "created_at": data[0]["created_at"],
        }])
    );
    assert_eq!(
        app.get("/api/v2/announcements", None).await.json::<Value>(),
        data
    );

    // Editing overwrites the author.
    let res = app
        .send_json(
            Method::PUT,
            &format!("/api/v2/announcements/{id}"),
            Some(&other_fc),
            &json!({"message": "Edited", "is_alert": false, "pages": "[\"/waitlist\"]"}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    let (name, data) = next_event(&mut sub).await;
    assert_eq!(name, events::ANNOUNCEMENT_UPDATED);
    assert_eq!(data[0]["message"], "Edited");
    assert_eq!(data[0]["pages"], "[\"/waitlist\"]");
    assert_eq!(data[0]["created_by"]["id"], 1002);

    let res = app
        .delete(&format!("/api/v2/announcements/{id}"), Some(&fc))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    let (name, data) = next_event(&mut sub).await;
    assert_eq!(name, events::ANNOUNCEMENT_UPDATED);
    assert_eq!(data, json!([]));

    // Gone: further edits and deletes are 400.
    let res = app
        .send_json(
            Method::PUT,
            &format!("/api/v2/announcements/{id}"),
            Some(&fc),
            &json!({"message": "x", "is_alert": false}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Announcment could not be found.")
    );
    let res = app
        .delete(&format!("/api/v2/announcements/{id}"), Some(&fc))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Announcment could not be found or has already been deleted."
        )
    );
    assert_no_event(&mut sub).await;
}

#[tokio::test]
async fn announcements_need_hq_fc() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let trainee = app.create_account(1001, "Trainee", Some("trainee")).await;
    let fc = app.create_account(1002, "Fc", Some("fc")).await;
    let body = json!({"message": "m", "is_alert": false});

    let res = app.post_json("/api/v2/announcements", None, &body).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (401, "401 Authorization Required")
    );
    let res = app
        .post_json("/api/v2/announcements", Some(&trainee), &body)
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    let res = app
        .send_json(
            Method::PUT,
            "/api/v2/announcements/1",
            Some(&trainee),
            &body,
        )
        .await;
    assert_eq!(res.status, 401);
    let res = app.delete("/api/v2/announcements/1", Some(&trainee)).await;
    assert_eq!(res.status, 401);

    // Malformed body.
    let res = app
        .post_json("/api/v2/announcements", Some(&fc), &json!({"message": "m"}))
        .await;
    assert_eq!(res.status, 422);
}

// --- badges ----------------------------------------------------------------------------------

async fn badge_id(app: &TestApp, name: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM badge WHERE name = ?")
        .bind(name)
        .fetch_one(app.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn badges_assign_list_and_revoke() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    app.create_character(2001, "Pilot").await;
    let logi = badge_id(&app, "LOGI").await;
    let alt = badge_id(&app, "ALT").await;
    sqlx::query("UPDATE badge SET exclude_badge_id = ? WHERE id = ?")
        .bind(alt)
        .bind(logi)
        .execute(app.pool())
        .await
        .unwrap();

    let res = app
        .post_json(
            &format!("/api/badges/{alt}/members"),
            Some(&fc),
            &json!({"id": 2001, "name": "ignored"}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));

    // Duplicate, exclusion, unknown badge, unknown character.
    let res = app
        .post_json(
            &format!("/api/badges/{alt}/members"),
            Some(&fc),
            &json!({"id": 2001}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Pilot already has ALT and cannot be assigned it a second time"
        )
    );
    let res = app
        .post_json(
            &format!("/api/badges/{logi}/members"),
            Some(&fc),
            &json!({"id": 2001}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Cannot assign LOGI to Pilot while they have been assigned ALT"
        )
    );
    let res = app
        .post_json("/api/badges/9999/members", Some(&fc), &json!({"id": 2001}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Badge not found (ID: 9999)")
    );
    let res = app
        .post_json(
            &format!("/api/badges/{alt}/members"),
            Some(&fc),
            &json!({"id": 3}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Character not found (ID: 3)")
    );

    let badges: Value = app.get("/api/badges", Some(&fc)).await.json();
    let alt_entry = badges
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "ALT")
        .unwrap();
    assert_eq!(
        alt_entry,
        &json!({"id": alt, "name": "ALT", "member_count": 1})
    );
    assert_eq!(badges.as_array().unwrap().len(), 3);

    let members: Value = app
        .get(&format!("/api/badges/{alt}/members"), Some(&fc))
        .await
        .json();
    assert_eq!(
        members,
        json!([{
            "badge": {"id": alt, "name": "ALT"},
            "character": {"id": 2001, "name": "Pilot"},
            "granted_by": {"id": 1001, "name": "Fc"},
            "granted_at": members[0]["granted_at"],
        }])
    );

    for _ in 0..2 {
        let res = app
            .delete(&format!("/api/badges/{alt}/members/2001"), Some(&fc))
            .await;
        assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    }
    let members: Value = app
        .get(&format!("/api/badges/{alt}/members"), Some(&fc))
        .await
        .json();
    assert_eq!(members, json!([]));
}

#[tokio::test]
async fn badges_need_badges_manage() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let trainee = app
        .create_account(1001, "Trainee", Some("trainee-advanced"))
        .await;
    for res in [
        app.get("/api/badges", Some(&trainee)).await,
        app.get("/api/badges/1/members", Some(&trainee)).await,
        app.post_json(
            "/api/badges/1/members",
            Some(&trainee),
            &json!({"id": 1001}),
        )
        .await,
        app.delete("/api/badges/1/members/1001", Some(&trainee))
            .await,
    ] {
        assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    }
}

// --- commanders ------------------------------------------------------------------------------

#[tokio::test]
async fn commanders_list_and_roles() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    let fc_plus = app.create_account(1002, "FcPlus", Some("fc+")).await;
    let admin = app.create_account(1003, "Admin", Some("admin")).await;

    let list: Value = app.get("/api/commanders", Some(&fc)).await.json();
    assert_eq!(list["commanders"].as_array().unwrap().len(), 3);
    assert_eq!(
        list["commanders"][0],
        json!({
            "character": {"id": 1001, "name": "Fc"},
            "role": "fc",
            "granted_by": {"id": 1001, "name": "Fc"},
            "granted_at": 0,
        })
    );
    assert_eq!(
        list["filters"],
        json!([
            {"name": "admin", "member_count": 1},
            {"name": "fc", "member_count": 1},
            {"name": "fc+", "member_count": 1},
        ])
    );

    let res = app.get("/api/commanders/roles", Some(&fc_plus)).await;
    assert_eq!(
        res.json::<Value>(),
        json!(["trainee", "trainee-advanced", "fc"])
    );
    let res = app.get("/api/commanders/roles", Some(&admin)).await;
    assert_eq!(
        res.json::<Value>(),
        json!(["trainee", "trainee-advanced", "fc", "fc+", "admin"])
    );

    // fc may view but not manage.
    let res = app.get("/api/commanders/roles", Some(&fc)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    let user = app.create_account(1004, "User", None).await;
    let res = app.get("/api/commanders", Some(&user)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
}

#[tokio::test]
async fn commanders_assign_lookup_revoke() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc_plus = app.create_account(1002, "FcPlus", Some("fc+")).await;
    app.create_account(1003, "Admin", Some("admin")).await;
    app.create_character(2001, "Pilot").await;

    let assign = |body: Value| {
        let app = &app;
        let login = fc_plus.clone();
        async move { app.post_json("/api/commanders", Some(&login), &body).await }
    };

    let res = assign(json!({"character_id": 2001, "role": "admin"})).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            403,
            "You do not have permission to grant the role \"admin\""
        )
    );
    let res = assign(json!({"character_id": 2001, "role": "boss"})).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "The FC rank \"boss\" does not exist")
    );
    let res = assign(json!({"role": "fc"})).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Request body missing required property character_id")
    );
    let res = assign(json!({"character_id": 9, "role": "fc"})).await;
    assert_eq!((res.status.as_u16(), res.text()), (404, ""));

    let res = assign(json!({"character_id": 2001, "role": "trainee"})).await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    let res = assign(json!({"character_id": 2001, "role": "fc"})).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "Cannot assign \"fc\" to Pilot as they already have a role"
        )
    );
    let granted_by: i64 =
        sqlx::query_scalar("SELECT granted_by_id FROM admin WHERE character_id = 2001")
            .fetch_one(app.pool())
            .await
            .unwrap();
    assert_eq!(granted_by, 1002);

    let res = app.get("/api/commanders/2001", Some(&fc_plus)).await;
    assert_eq!((res.status.as_u16(), res.text()), (200, "trainee"));
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("text/plain")
    );
    let res = app.get("/api/commanders/9", Some(&fc_plus)).await;
    assert_eq!((res.status.as_u16(), res.text()), (404, ""));

    let res = app.delete("/api/commanders/1002", Some(&fc_plus)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "You cannot revoke your own rank.")
    );
    let res = app.delete("/api/commanders/1003", Some(&fc_plus)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            403,
            "You do not have permission to revoke the role \"admin\""
        )
    );
    for _ in 0..2 {
        let res = app.delete("/api/commanders/2001", Some(&fc_plus)).await;
        assert_eq!((res.status.as_u16(), res.text()), (200, "Ok"));
    }
    let res = app.get("/api/commanders/2001", Some(&fc_plus)).await;
    assert_eq!(res.status, 404);
}

// --- notes -----------------------------------------------------------------------------------

#[tokio::test]
async fn notes_add_list_and_toggle() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    app.create_character(2001, "Pilot").await;
    sqlx::query(
        "INSERT INTO waitlist (id, name, is_open, is_archived) VALUES (101, 'open', 1, 0), (102, 'closed', 0, 0)",
    )
    .execute(app.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO waitlist_entry (waitlist_id, account_id, joined_at) VALUES (101, 2001, 0), (102, 2001, 0)",
    )
    .execute(app.pool())
    .await
    .unwrap();

    let res = app
        .post_json(
            "/api/notes/add",
            Some(&fc),
            &json!({"character_id": 2001, "note": "too short"}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (400, "Invalid note"));
    for note in ["first note here", "second note here"] {
        let res = app
            .post_json(
                "/api/notes/add",
                Some(&fc),
                &json!({"character_id": 2001, "note": note}),
            )
            .await;
        assert_eq!((res.status.as_u16(), res.text()), (200, "OK"));
    }

    let notes: Value = app
        .get("/api/notes?character_id=2001", Some(&fc))
        .await
        .json();
    let notes = notes["notes"].as_array().unwrap().clone();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0]["note"], "first note here");
    assert_eq!(notes[0]["author"], json!({"id": 1001, "name": "Fc"}));
    assert_eq!(notes[0]["show_on_waitlist"], false);
    let (first, second) = (
        notes[0]["id"].as_i64().unwrap(),
        notes[1]["id"].as_i64().unwrap(),
    );

    let mut sub = app.live().subscribe(vec![topics::WAITLIST.to_string()]);
    for id in [first, second] {
        let res = app
            .post_json(
                "/api/notes/toggle_waitlist",
                Some(&fc),
                &json!({"note_id": id, "show_on_waitlist": true}),
            )
            .await;
        assert_eq!((res.status.as_u16(), res.text()), (200, "OK"));
        // Only the open waitlist is notified.
        let (name, data) = next_event(&mut sub).await;
        assert_eq!(name, events::WAITLIST_UPDATE);
        assert_eq!(data, json!({"waitlist_id": 101}));
        assert_no_event(&mut sub).await;
    }
    // At most one pinned note per character.
    let notes: Value = app
        .get("/api/notes?character_id=2001", Some(&fc))
        .await
        .json();
    assert_eq!(notes["notes"][0]["show_on_waitlist"], false);
    assert_eq!(notes["notes"][1]["show_on_waitlist"], true);

    let res = app
        .post_json(
            "/api/notes/toggle_waitlist",
            Some(&fc),
            &json!({"note_id": 999, "show_on_waitlist": false}),
        )
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (400, "Note not found"));
}

#[tokio::test]
async fn notes_need_permissions() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let trainee = app
        .create_account(1001, "T", Some("trainee-advanced"))
        .await;
    for res in [
        app.get("/api/notes?character_id=1001", Some(&trainee))
            .await,
        app.post_json(
            "/api/notes/add",
            Some(&trainee),
            &json!({"character_id": 1001, "note": "a long enough note"}),
        )
        .await,
        app.post_json(
            "/api/notes/toggle_waitlist",
            Some(&trainee),
            &json!({"note_id": 1, "show_on_waitlist": true}),
        )
        .await,
    ] {
        assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    }
}

// --- local search ----------------------------------------------------------------------------

#[tokio::test]
async fn search_finds_known_characters() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    app.create_character(2001, "Alpha Pilot").await;
    app.create_character(2002, "Beta Pilot").await;
    app.create_character(2003, "Gamma").await;

    let res: Value = app.get("/api/search?query=Pilot", Some(&fc)).await.json();
    assert_eq!(
        res,
        json!({
            "query": "Pilot",
            "results": [{"id": 2001, "name": "Alpha Pilot"}, {"id": 2002, "name": "Beta Pilot"}],
        })
    );

    let trainee = app
        .create_account(1002, "T", Some("trainee-advanced"))
        .await;
    let res = app.get("/api/search?query=Pilot", Some(&trainee)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    // Missing query parameter: legacy answered "no route matches" (404).
    let res = app.get("/api/search", Some(&fc)).await;
    assert_eq!((res.status.as_u16(), res.text()), (404, "404 Not Found"));
}
