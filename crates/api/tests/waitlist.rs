//! `/api/waitlist*`: list, x-up, approve/reject, open/empty, removal and invites.

mod common;
mod support;

use std::collections::HashSet;

use axum::http::StatusCode;
use common::Login;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use support::{Env, next_event, no_event};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, ResponseTemplate};
use wl_live::{events, topics};

fn xup_body(character_id: i64, dna: &[(i64, String)]) -> Value {
    json!({
        "waitlist_id": 1,
        "character_id": character_id,
        "eft": "",
        "is_alt": false,
        "messagexup": "hi",
        "dna": dna.iter().map(|(id, dna)| json!({"character_id": id, "dna": dna})).collect::<Vec<_>>(),
    })
}

/// X-up `suffix` (a doctrine fit) as `character_id`; returns the response body.
async fn xup(env: &Env, login: &Login, character_id: i64, suffix: &str) -> Value {
    let res = env
        .post_json(
            "/api/waitlist/xup",
            Some(login),
            &xup_body(character_id, &[(character_id, env.dna(suffix))]),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()
}

async fn list(env: &Env, login: &Login) -> Value {
    let res = env.get("/api/waitlist?waitlist_id=1", Some(login)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()
}

async fn fit_ids(env: &Env) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM waitlist_entry_fit ORDER BY id")
        .fetch_all(env.pool())
        .await
        .unwrap()
}

async fn count(env: &Env, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
        .fetch_one(env.pool())
        .await
        .unwrap()
}

async fn put_at_war(env: &Env) {
    env.state
        .war_cache()
        .replace(HashSet::from([support::CORPORATION_ID]), HashSet::new());
}

#[tokio::test]
async fn list_requires_login_and_reports_closed_lists() {
    let Some(env) = Env::new().await else { return };
    let res = env.get("/api/waitlist?waitlist_id=1", None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let me = env.pilot(1001, None).await;
    let expected =
        json!({"open": false, "waitlist": null, "categories": ["Logistics", "Bastion", "Other"]});
    assert_eq!(list(&env, &me).await, expected);
    // A waitlist that doesn't exist looks closed.
    let res = env.get("/api/waitlist?waitlist_id=99", Some(&me)).await;
    assert_eq!(res.json::<Value>(), expected);
}

#[tokio::test]
async fn set_open_needs_waitlist_edit_and_notifies() {
    let Some(env) = Env::new().await else { return };
    let trainee = env.pilot(1001, Some("trainee-advanced")).await;
    let fc = env.pilot(1002, Some("fc")).await;
    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);

    let body = json!({"open": true, "waitlist_id": 1});
    let res = env
        .post_json("/api/waitlist/set_open", Some(&trainee), &body)
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );

    let res = env
        .post_json("/api/waitlist/set_open", Some(&fc), &body)
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    let event = next_event(&mut sub).await;
    assert_eq!(event.name, events::WAITLIST_UPDATE);
    assert_eq!(event.data, r#"{"waitlist_id":1}"#);
    assert_eq!(list(&env, &fc).await["open"], json!(true));
    assert_eq!(list(&env, &fc).await["waitlist"], json!([]));
}

#[tokio::test]
async fn xup_stores_fits_and_notifies_fleet_bosses() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    // Two fleets bossed by one FC (main and alt): one message, to the FC's account.
    let boss = env.pilot(2001, Some("fc")).await;
    env.alt(2001, 2002).await;
    sqlx::query("INSERT INTO fleet (id, boss_id) VALUES (77, 2001), (78, 2002)")
        .execute(env.pool())
        .await
        .unwrap();
    let me = env.pilot(1001, None).await;
    env.alt(1001, 1002).await;
    let mut sub = env.live().subscribe(vec![
        topics::WAITLIST.into(),
        topics::account(boss.account_id),
    ]);

    // One EFT fit for the main plus one DNA fit for the alt.
    let body = json!({
        "waitlist_id": 1,
        "character_id": 1001,
        "eft": "[Vindicator, my vindi]\n",
        "is_alt": true,
        "messagexup": "two ships",
        "dna": [{"character_id": 1002, "dna": env.dna("ARMOR_PALADIN")}],
    });
    let res = env.post_json("/api/waitlist/xup", Some(&me), &body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json::<Value>(), json!({"status": "ok", "warnings": []}));

    let message = next_event(&mut sub).await;
    assert_eq!(message.topic, topics::account(boss.account_id));
    assert_eq!(message.name, events::MESSAGE);
    assert_eq!(message.data, r#"{"message":"New x-up in waitlist"}"#);
    let update = next_event(&mut sub).await;
    assert_eq!(
        (update.name.as_str(), update.data.as_str()),
        (events::WAITLIST_UPDATE, r#"{"waitlist_id":1}"#)
    );

    assert_eq!(count(&env, "fit_history").await, 2);
    let view = list(&env, &me).await;
    let entries = view["waitlist"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(
        entry["character"],
        json!({"id": 1001, "name": "Pilot 1001"})
    );
    assert_eq!(entry["can_remove"], json!(true));
    let fits = entry["fits"].as_array().unwrap();
    assert_eq!(fits.len(), 2);
    let vindi = &fits[0];
    assert_eq!(
        vindi["hull"],
        json!({"id": env.type_id("Vindicator"), "name": "Vindicator"})
    );
    assert_eq!(vindi["category"], json!("Other"));
    assert_eq!(vindi["approved"], json!(false));
    assert_eq!(vindi["is_alt"], json!(true));
    assert_eq!(vindi["messagexup"], json!("two ships"));
    assert_eq!(vindi["hours_in_fleet"], json!(0));
    assert_eq!(vindi["implants"], json!([]));
    let paladin = &fits[1];
    assert_eq!(
        paladin["character"],
        json!({"id": 1002, "name": "Pilot 1002"})
    );
    assert_eq!(paladin["category"], json!("Bastion"));
    assert_eq!(paladin["approved"], json!(true));
    assert_eq!(paladin["dna"], json!(env.dna("ARMOR_PALADIN")));
    assert_eq!(paladin["fit_analysis"]["name"], json!("ARMOR_PALADIN"));
}

#[tokio::test]
async fn list_hides_fields_by_permission() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    put_at_war(&env).await; // gives the fit a non-public tag (AT-WAR)
    let res = xup(&env, &pilot, 1001, "ARMOR_PALADIN").await;
    assert_eq!(
        res["warnings"],
        json!([
            "Your corporation or alliance is at war. You may still x-up Bastion/Other, but Logistics is blocked."
        ])
    );
    sqlx::query("INSERT INTO character_note (character_id, author_id, note, logged_at, show_on_waitlist) VALUES (1001, 1001, 'pinned', 0, 1)")
        .execute(env.pool())
        .await
        .unwrap();

    let other = env.pilot(3001, None).await;
    let trainee = env.pilot(3002, Some("trainee")).await;
    let fc = env.pilot(3003, Some("fc")).await;

    let entry = |v: Value| v["waitlist"][0].clone();
    let stranger = entry(list(&env, &other).await);
    assert_eq!(stranger["character"], Value::Null);
    assert_eq!(stranger["can_remove"], json!(false));
    assert_eq!(stranger["waitlist_note"], Value::Null);
    let fit = &stranger["fits"][0];
    assert_eq!(fit["tags"], json!([]));
    for field in [
        "character",
        "hours_in_fleet",
        "review_comment",
        "dna",
        "implants",
        "fit_analysis",
        "messagexup",
    ] {
        assert_eq!(fit[field], Value::Null, "{field}");
    }
    assert_eq!(fit["approved"], json!(false));

    // waitlist-view without fit-view: people and tags, no fit details.
    let seen = entry(list(&env, &trainee).await);
    assert_eq!(seen["character"]["id"], json!(1001));
    assert_eq!(seen["can_remove"], json!(false));
    assert_eq!(seen["waitlist_note"], json!("pinned"));
    assert_eq!(seen["fits"][0]["tags"], json!(["AT-WAR"]));
    assert_eq!(seen["fits"][0]["messagexup"], json!("hi"));
    assert_eq!(seen["fits"][0]["dna"], Value::Null);

    let full = entry(list(&env, &fc).await);
    assert_eq!(full["can_remove"], json!(true));
    assert_eq!(full["fits"][0]["dna"], json!(env.dna("ARMOR_PALADIN")));
    assert!(full["fits"][0]["fit_analysis"].is_object());

    // The owner sees everything but the pinned note.
    let own = entry(list(&env, &pilot).await);
    assert_eq!(own["waitlist_note"], Value::Null);
    assert_eq!(own["fits"][0]["tags"], json!(["AT-WAR"]));
    assert!(own["fits"][0]["dna"].is_string());
}

#[tokio::test]
async fn xup_validation_errors() {
    let Some(env) = Env::new().await else { return };
    let me = env.pilot(1001, None).await;
    env.pilot(1002, None).await; // someone else's character
    let paladin = env.dna("ARMOR_PALADIN");
    let post = |body: Value| {
        let env = &env;
        let me = me.clone();
        async move {
            let res = env.post_json("/api/waitlist/xup", Some(&me), &body).await;
            (res.status, res.text().to_string())
        }
    };
    let bad = |msg: &str| (StatusCode::BAD_REQUEST, msg.to_string());

    assert_eq!(
        post(xup_body(1001, &[(1001, paladin.clone())])).await,
        bad("Waitlist is closed")
    );
    env.open_waitlist().await;
    assert_eq!(post(xup_body(1001, &[])).await, bad("No fits supplied"));
    let eleven: Vec<_> = (0..11).map(|_| (1001, paladin.clone())).collect();
    assert_eq!(post(xup_body(1001, &eleven)).await, bad("Too many fits"));
    let mut long = xup_body(1001, &[(1001, paladin.clone())]);
    long["messagexup"] = json!("x".repeat(101));
    assert_eq!(post(long).await, bad("X-up message too long!"));
    assert_eq!(
        post(xup_body(1001, &[(1001, "nonsense".into())])).await,
        bad("fit error: fit failed to parse")
    );
    assert_eq!(
        post(xup_body(1001, &[(1002, paladin.clone())])).await,
        (StatusCode::UNAUTHORIZED, "access denied".into())
    );
    let mut missing = xup_body(1001, &[]);
    missing.as_object_mut().unwrap().remove("messagexup");
    assert_eq!(post(missing).await.0, StatusCode::UNPROCESSABLE_ENTITY);

    // A pilot without skills: the fit checker's first error.
    env.mock_skills(1001, &Default::default()).await;
    sqlx::query("DELETE FROM skill_current")
        .execute(env.pool())
        .await
        .unwrap();
    env.esi.reset().await;
    env.mock_character(1001, "Pilot 1001").await;
    env.mock_implants(1001, &[]).await;
    env.mock_skills(1001, &Default::default()).await;
    let (status, text) = post(xup_body(1001, &[(1001, paladin.clone())])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.starts_with("Missing skills to online/use"), "{text}");

    // Without the skills scope.
    env.grant_tokens(1001, &["publicData"]).await;
    assert_eq!(
        post(xup_body(1001, &[(1001, paladin.clone())])).await,
        (
            StatusCode::UNAUTHORIZED,
            "ESI error: missing ESI scope".into()
        )
    );
    assert_eq!(count(&env, "waitlist_entry").await, 0);
}

#[tokio::test]
async fn xup_rejects_banned_pilots() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let me = env.pilot(1001, None).await;
    for (entity, kind, expected) in [
        (
            support::CORPORATION_ID,
            "Corporation",
            "You cannot join fleet as your corporation is banned.",
        ),
        (
            1001,
            "Character",
            "You cannot join fleet as your character is banned.",
        ),
    ] {
        sqlx::query("INSERT INTO ban (entity_id, entity_type, issued_at, issued_by, reason) VALUES (?, ?, 0, 1001, 'r')")
            .bind(entity)
            .bind(kind)
            .execute(env.pool())
            .await
            .unwrap();
        let res = env
            .post_json(
                "/api/waitlist/xup",
                Some(&me),
                &xup_body(1001, &[(1001, env.dna("ARMOR_PALADIN"))]),
            )
            .await;
        assert_eq!(
            (res.status, res.text()),
            (StatusCode::BAD_REQUEST, expected)
        );
    }
}

#[tokio::test]
async fn xup_war_rules_block_logistics() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let me = env.pilot(1001, None).await;
    put_at_war(&env).await;
    let res = env
        .post_json(
            "/api/waitlist/xup",
            Some(&me),
            &xup_body(1001, &[(1001, env.dna("ARMOR_NESTOR"))]),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (
            StatusCode::BAD_REQUEST,
            "You cannot x-up Logistics while your corporation or alliance is at war."
        )
    );
    assert_eq!(count(&env, "waitlist_entry_fit").await, 0);

    xup(&env, &me, 1001, "ARMOR_PALADIN").await;
    let (approved, tags): (i8, String) =
        sqlx::query_as("SELECT approved, tags FROM waitlist_entry_fit")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!((approved, tags.as_str()), (0, "AT-WAR"));
}

#[tokio::test]
async fn xup_replaces_the_same_hull_and_caps_fits_per_entry() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let me = env.pilot(1001, None).await;
    xup(&env, &me, 1001, "ARMOR_PALADIN").await;
    let first = fit_ids(&env).await;
    xup(&env, &me, 1001, "ARMOR_PALADIN").await;
    let second = fit_ids(&env).await;
    assert_eq!(second.len(), 1);
    assert_ne!(first, second);
    // Dedup: still one fitting and implant set, two history rows.
    assert_eq!(count(&env, "fitting").await, 1);
    assert_eq!(count(&env, "implant_set").await, 1);
    assert_eq!(count(&env, "fit_history").await, 2);

    // 1 existing + 10 new > 10.
    let ten: Vec<_> = (0..10).map(|_| (1001, env.dna("ARMOR_NESTOR"))).collect();
    let res = env
        .post_json("/api/waitlist/xup", Some(&me), &xup_body(1001, &ten))
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::BAD_REQUEST, "Too many fits")
    );
    assert_eq!(count(&env, "waitlist_entry_fit").await, 1);
}

#[tokio::test]
async fn approve_and_reject() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let manager = env.pilot(2001, Some("trainee-advanced")).await;
    let trainee = env.pilot(2002, Some("trainee")).await;
    xup(&env, &pilot, 1001, "ARMOR_NESTOR").await;
    let id = fit_ids(&env).await[0];
    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);

    let res = env
        .post_json("/api/waitlist/approve", Some(&trainee), &json!({"id": id}))
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );

    let res = env
        .post_json(
            "/api/waitlist/reject",
            Some(&manager),
            &json!({"id": id, "review_comment": "fix it"}),
        )
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    assert_eq!(next_event(&mut sub).await.data, r#"{"waitlist_id":1}"#);
    let fit = &list(&env, &pilot).await["waitlist"][0]["fits"][0];
    assert_eq!(
        (fit["approved"].clone(), fit["review_comment"].clone()),
        (json!(false), json!("fix it"))
    );

    let res = env
        .post_json("/api/waitlist/approve", Some(&manager), &json!({"id": id}))
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    next_event(&mut sub).await;
    assert_eq!(
        list(&env, &pilot).await["waitlist"][0]["fits"][0]["approved"],
        json!(true)
    );

    let res = env
        .post_json(
            "/api/waitlist/approve",
            Some(&manager),
            &json!({"id": 999_999}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::INTERNAL_SERVER_ERROR, "database error")
    );
    let res = env
        .post_json("/api/waitlist/reject", Some(&manager), &json!({"id": id}))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn remove_fit_and_remove_x() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let stranger = env.pilot(1002, None).await;
    let manager = env.pilot(2001, Some("trainee-advanced")).await;
    xup(&env, &pilot, 1001, "ARMOR_PALADIN").await;
    xup(&env, &pilot, 1001, "ARMOR_NESTOR").await;
    let fits = fit_ids(&env).await;
    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);

    let res = env
        .post_json(
            "/api/waitlist/remove_fit",
            Some(&stranger),
            &json!({"id": fits[0]}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );

    let res = env
        .post_json(
            "/api/waitlist/remove_fit",
            Some(&pilot),
            &json!({"id": fits[0]}),
        )
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    next_event(&mut sub).await;
    assert_eq!(count(&env, "waitlist_entry").await, 1);
    // Removing the last fit removes the entry.
    let res = env
        .post_json(
            "/api/waitlist/remove_fit",
            Some(&manager),
            &json!({"id": fits[1]}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    next_event(&mut sub).await;
    assert_eq!(count(&env, "waitlist_entry").await, 0);

    xup(&env, &pilot, 1001, "ARMOR_PALADIN").await;
    xup(&env, &pilot, 1001, "ARMOR_NESTOR").await;
    next_event(&mut sub).await;
    next_event(&mut sub).await;
    let entry: i64 = sqlx::query_scalar("SELECT id FROM waitlist_entry")
        .fetch_one(env.pool())
        .await
        .unwrap();
    let res = env
        .post_json(
            "/api/waitlist/remove_x",
            Some(&stranger),
            &json!({"id": entry}),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = env
        .post_json(
            "/api/waitlist/remove_x",
            Some(&pilot),
            &json!({"id": entry}),
        )
        .await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    assert_eq!(next_event(&mut sub).await.name, events::WAITLIST_UPDATE);
    assert_eq!(count(&env, "waitlist_entry_fit").await, 0);
    assert_eq!(count(&env, "waitlist_entry").await, 0);
    let res = env
        .post_json(
            "/api/waitlist/remove_x",
            Some(&pilot),
            &json!({"id": entry}),
        )
        .await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn empty_needs_a_closed_list_and_is_silent() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    let fc = env.pilot(2001, Some("fc")).await;
    xup(&env, &pilot, 1001, "ARMOR_PALADIN").await;
    let body = json!({"waitlist_id": 1});

    let res = env
        .post_json("/api/waitlist/empty", Some(&pilot), &body)
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    let res = env.post_json("/api/waitlist/empty", Some(&fc), &body).await;
    assert_eq!(
        (res.status, res.text()),
        (
            StatusCode::BAD_REQUEST,
            "Waitlist must be closed in order to empty it"
        )
    );

    sqlx::query("UPDATE waitlist SET is_open = 0")
        .execute(env.pool())
        .await
        .unwrap();
    let mut sub = env.live().subscribe(vec![topics::WAITLIST.into()]);
    let res = env.post_json("/api/waitlist/empty", Some(&fc), &body).await;
    assert_eq!((res.status, res.text()), (StatusCode::OK, "OK"));
    no_event(&mut sub).await;
    assert_eq!(count(&env, "waitlist_entry").await, 0);
    assert_eq!(count(&env, "waitlist_entry_fit").await, 0);
}

#[tokio::test]
async fn invite_uses_the_category_squad_and_wakes_the_pilot() {
    let Some(env) = Env::new().await else { return };
    env.open_waitlist().await;
    let pilot = env.pilot(1001, None).await;
    env.alt(1001, 1002).await;
    let fc = env.pilot(2001, Some("trainee")).await;
    let user = env.pilot(2002, None).await;
    xup(&env, &pilot, 1001, "ARMOR_PALADIN").await;
    let fit = fit_ids(&env).await[0];
    let body = json!({"id": fit, "character_id": 2001});

    let res = env
        .post_json(
            "/api/waitlist/invite",
            Some(&user),
            &json!({"id": fit, "character_id": 2002}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = env
        .post_json(
            "/api/waitlist/invite",
            Some(&fc),
            &json!({"id": fit, "character_id": 2002}),
        )
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::UNAUTHORIZED, "access denied")
    );
    let res = env
        .post_json("/api/waitlist/invite", Some(&fc), &body)
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::BAD_REQUEST, "Fleet not configured")
    );

    sqlx::query("INSERT INTO fleet (id, boss_id) VALUES (77, 2001)")
        .execute(env.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO fleet_squad (fleet_id, category, wing_id, squad_id) VALUES (77, 'bastion', 10, 11), (77, 'alt', 20, 21)")
        .execute(env.pool())
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/fleets/77/members"))
        .and(body_partial_json(
            json!({"character_id": 1001, "role": "squad_member", "wing_id": 10, "squad_id": 11}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&env.esi)
        .await;
    let mut sub = env
        .live()
        .subscribe(vec![topics::account(1001), topics::WAITLIST.into()]);
    let res = env
        .post_json("/api/waitlist/invite", Some(&fc), &body)
        .await;
    assert_eq!(
        (res.status, res.text()),
        (StatusCode::OK, "OK"),
        "{}",
        res.text()
    );
    let wakeup = next_event(&mut sub).await;
    assert_eq!(wakeup.topic, "account;1001");
    assert_eq!(wakeup.name, events::WAKEUP);
    assert_eq!(wakeup.data, "Pilot 2001 has invited your Paladin to fleet.");
    no_event(&mut sub).await; // no waitlist_update
    assert_eq!(count(&env, "waitlist_entry_fit").await, 1);

    // Alt x-ups go to the `alt` squad; ESI refusals keep their status.
    sqlx::query("UPDATE waitlist_entry_fit SET is_alt = 1")
        .execute(env.pool())
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/fleets/77/members"))
        .and(body_partial_json(json!({"wing_id": 20, "squad_id": 21})))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(
                json!({"error": "Character is already in a fleet, character_id: 1001"}),
            ),
        )
        .mount(&env.esi)
        .await;
    let res = env
        .post_json("/api/waitlist/invite", Some(&fc), &body)
        .await;
    assert_eq!(
        (res.status, res.text()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "ESI error: Character is already in a fleet"
        )
    );
}
