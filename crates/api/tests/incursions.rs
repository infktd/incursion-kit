//! The public incursion tracker API (`docs/incursions-api.md`): shapes, pagination, windows and
//! the anonymous live stream. Enrichment checks need the static data file with the star map and
//! are skipped without it.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::TestApp;
use futures::StreamExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wl_eve_data::EveData;
use wl_live::{events, topics};

/// Claysson's constellation and its recorded layout.
const VALE: i64 = 20000396;
const CLAYSSON: i64 = 30002708;

async fn eve() -> Option<Arc<EveData>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite");
    if !path.exists() {
        return None;
    }
    let eve = EveData::load(&path).await.ok()?;
    (!eve.universe().is_empty()).then(|| Arc::new(eve))
}

async fn app() -> Option<(TestApp, bool)> {
    match eve().await {
        Some(eve) => Some((
            TestApp::with_services(|state, _, _| state.with_eve_data(eve)).await?,
            true,
        )),
        None => {
            eprintln!("enrichment checks skipped: no static data file with a star map");
            Some((TestApp::new().await?, false))
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Inserts a spawn; returns its id.
#[allow(clippy::too_many_arguments)]
async fn spawn(
    app: &TestApp,
    constellation: (i64, &str),
    staging: (i64, &str, f64),
    band: &str,
    state: &str,
    first_seen_at: i64,
    state_changed_at: i64,
    ended_at: Option<i64>,
) -> i64 {
    sqlx::query(
        "INSERT INTO incursion_spawn (constellation_id, constellation_name, region_id, \
         region_name, faction_id, incursion_type, staging_system_id, staging_system_name, \
         staging_security, security_band, state, influence, has_boss, first_seen_at, \
         observed_from_start, last_seen_at, state_changed_at, ended_at) \
         VALUES (?, ?, 10000032, 'Sinq Laison', 500019, 'Incursion', ?, ?, ?, ?, ?, 0.25, 0, \
         ?, 1, ?, ?, ?)",
    )
    .bind(constellation.0)
    .bind(constellation.1)
    .bind(staging.0)
    .bind(staging.1)
    .bind(staging.2)
    .bind(band)
    .bind(state)
    .bind(first_seen_at)
    .bind(ended_at.unwrap_or(now()))
    .bind(state_changed_at)
    .bind(ended_at)
    .execute(app.pool())
    .await
    .unwrap()
    .last_insert_id() as i64
}

async fn exec(app: &TestApp, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(app.pool())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// An active high-sec spawn with its layout, samples and events.
async fn active_spawn(app: &TestApp) -> i64 {
    let t = now();
    let id = spawn(
        app,
        (VALE, "Vale"),
        (CLAYSSON, "Claysson", 0.937687),
        "highsec",
        "established",
        t - 7200,
        t - 7200,
        None,
    )
    .await;
    let layout = &wl_incursions::roles::known_layouts()[&VALE];
    let mut systems = vec![format!(
        "({id}, {CLAYSSON}, 'Claysson', 0.937687, 'staging', 'known')"
    )];
    for (ids, role) in [
        (&layout.vanguard, "vanguard"),
        (&layout.assault, "assault"),
        (&layout.headquarters, "headquarters"),
    ] {
        for system in ids {
            systems.push(format!(
                "({id}, {system}, 'S{system}', 0.8, '{role}', 'known')"
            ));
        }
    }
    exec(
        app,
        &format!(
            "INSERT INTO incursion_spawn_system (spawn_id, system_id, system_name, security, role, \
             role_source) VALUES {}",
            systems.join(", ")
        ),
    )
    .await;
    // Samples every 5 minutes for the last two hours.
    let samples: Vec<String> = (0..24)
        .map(|i| {
            format!(
                "({id}, {}, {})",
                t - 7200 + i * 300,
                f64::from(i as i32) / 100.0
            )
        })
        .collect();
    exec(
        app,
        &format!(
            "INSERT INTO incursion_influence (spawn_id, sampled_at, influence) VALUES {}",
            samples.join(", ")
        ),
    )
    .await;
    exec(
        app,
        &format!(
            "INSERT INTO incursion_event (spawn_id, at, kind, to_state) \
             VALUES ({id}, {}, 'spawned', 'established')",
            t - 7200
        ),
    )
    .await;
    id
}

#[tokio::test]
async fn active_incursions_are_public_and_enriched() {
    let Some((app, enriched)) = app().await else {
        return;
    };
    let id = active_spawn(&app).await;
    exec(
        &app,
        &format!(
            "INSERT INTO incursion_tracker_state (id, last_poll_at) VALUES (1, {})",
            now()
        ),
    )
    .await;

    let res = app.get("/api/incursions", None).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.header("cache-control"), Some("public, max-age=60"));
    let body: Value = res.json();
    assert!(body["last_poll_at"].is_string());
    assert!(body["data_modified_at"].is_null());
    let list = body["incursions"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    let spawn = &list[0];
    assert_eq!(spawn["id"], id);
    assert_eq!(spawn["active"], true);
    assert_eq!(spawn["constellation"], json!({"id": VALE, "name": "Vale"}));
    assert_eq!(
        spawn["region"],
        json!({"id": 10000032, "name": "Sinq Laison"})
    );
    assert_eq!(
        spawn["faction"],
        json!({"id": 500019, "name": "Sansha's Nation"})
    );
    assert_eq!(spawn["type"], "Incursion");
    assert_eq!(spawn["security_band"], "highsec");
    assert_eq!(spawn["staging_system"]["security"], 0.9);
    assert_eq!(spawn["state"], "established");
    assert!(spawn["ended_at"].is_null());
    assert!(spawn.get("events").is_none(), "only the detail has events");

    // Established 2 hours ago: at most 8 days in total.
    let lifetime = &spawn["lifetime"];
    assert_eq!(lifetime["state_since_observed"], true);
    let remaining = lifetime["max_remaining_seconds"].as_i64().unwrap();
    assert!(
        (8 * 86400 - 7200 - 60..=8 * 86400 - 7200).contains(&remaining),
        "{remaining}"
    );

    // Hourly influence for the list: one sample per hour bucket.
    let history = spawn["influence_history"].as_array().unwrap();
    assert!((2..=3).contains(&history.len()), "{history:?}");
    assert_eq!(history.last().unwrap()[1], 0.23);

    let systems = spawn["systems"].as_array().unwrap();
    let roles: Vec<&str> = systems
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles.first(), Some(&"staging"));
    assert_eq!(roles.last(), Some(&"headquarters"));
    assert!(roles.windows(2).all(|w| {
        let order = ["staging", "vanguard", "assault", "headquarters", "unknown"];
        order.iter().position(|r| *r == w[0]) <= order.iter().position(|r| *r == w[1])
    }));
    assert_eq!(systems[0]["role_source"], "known");

    let references = spawn["reference_jumps"].as_array().unwrap();
    if enriched {
        assert_eq!(systems[0]["jumps_from_staging"], 0);
        assert!(systems[0]["radius_au"].as_f64().unwrap() > 1.0);
        assert!(
            systems
                .iter()
                .any(|s| !s["stations"].as_array().unwrap().is_empty()),
            "Vale has NPC stations"
        );
        let station = systems
            .iter()
            .flat_map(|s| s["stations"].as_array().unwrap())
            .next()
            .unwrap();
        assert!(station["name"].as_str().unwrap().contains(" - "));
        assert!(station["has_repair"].is_boolean());
        assert_eq!(references.len(), 5, "the default reference systems");
        let jita = &references[0];
        assert_eq!(jita["system_name"], "Jita");
        assert!(jita["jumps"].as_u64().unwrap() > 0);
        assert!(jita["highsec_jumps"].as_u64().unwrap() >= jita["jumps"].as_u64().unwrap());
    } else {
        assert!(systems[0]["jumps_from_staging"].is_null());
        assert!(references.is_empty());
    }
}

#[tokio::test]
async fn one_incursion_has_its_full_history() {
    let Some((app, _)) = app().await else { return };
    let id = active_spawn(&app).await;

    let res = app.get(&format!("/api/incursions/{id}"), None).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.header("cache-control"), Some("public, max-age=60"));
    let spawn: Value = res.json();
    assert_eq!(spawn["influence_history"].as_array().unwrap().len(), 24);
    assert_eq!(spawn["influence_history"][0][1], 0.0);
    let events = spawn["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["kind"], "spawned");
    assert_eq!(events[0]["to_state"], "established");
    assert!(events[0]["at"].as_str().unwrap().ends_with('Z'));

    for path in ["/api/incursions/999999", "/api/incursions/abc"] {
        let res = app.get(path, None).await;
        assert_eq!(res.status, 404, "{path}");
        assert_eq!(res.text(), "incursion not found");
    }
}

#[tokio::test]
async fn history_is_paginated_and_filtered() {
    let Some((app, _)) = app().await else { return };
    let t = now();
    active_spawn(&app).await;
    for i in 0..3 {
        spawn(
            &app,
            (20000001 + i, &format!("Old {i}")),
            (30000001 + i, "Somewhere", -0.4),
            "nullsec",
            "withdrawing",
            t - 86400 * (10 - i),
            t - 86400 * (9 - i),
            Some(t - 86400 * (8 - i)),
        )
        .await;
    }
    spawn(
        &app,
        (20000100, "Low"),
        (30000100, "Lowish", 0.3),
        "lowsec",
        "mobilizing",
        t - 86400 * 3,
        t - 86400 * 2,
        Some(t - 3600),
    )
    .await;

    let page: Value = app
        .get("/api/incursions/history?per_page=2", None)
        .await
        .json();
    assert_eq!(page["total"], 4);
    assert_eq!(page["page"], 1);
    assert_eq!(page["per_page"], 2);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0]["constellation"]["name"], "Low",
        "most recent end first"
    );
    assert_eq!(items[0]["state"], "ended");
    assert_eq!(items[0]["active"], false);
    assert!(items[0]["ended_at"].is_string());
    assert_eq!(items[0]["duration_seconds"], 86400 * 3 - 3600);
    assert!(items[0].get("systems").is_none());

    let page: Value = app
        .get(
            "/api/incursions/history?band=nullsec&page=2&per_page=2",
            None,
        )
        .await
        .json();
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["constellation"]["name"], "Old 0");

    for bad in ["band=wspace", "per_page=0", "per_page=101"] {
        let res = app
            .get(&format!("/api/incursions/history?{bad}"), None)
            .await;
        assert_eq!(res.status, 400, "{bad}");
    }

    let log: Value = app.get("/api/incursions/log?limit=1", None).await.json();
    let items = log["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "spawned");
    assert_eq!(items[0]["constellation"]["name"], "Vale");
    assert_eq!(items[0]["staging_system"]["name"], "Claysson");
    let before = log["next_before"].as_i64().unwrap();
    let older: Value = app
        .get(&format!("/api/incursions/log?before={before}"), None)
        .await
        .json();
    assert!(older["items"].as_array().unwrap().is_empty());
    assert!(older["next_before"].is_null());
}

#[tokio::test]
async fn spawn_windows_follow_recent_ends() {
    let Some((app, _)) = app().await else { return };
    let t = now();
    active_spawn(&app).await; // high-sec: its only slot is taken
    // Two null-sec ends: 2 hours ago (window pending) and 20 hours ago (window open). A third
    // null-sec slot has no recorded end.
    for (i, ended) in [(0, t - 2 * 3600), (1, t - 20 * 3600), (2, t - 50 * 3600)] {
        spawn(
            &app,
            (20000001 + i, &format!("Null {i}")),
            (30000001 + i, "Somewhere", -0.4),
            "nullsec",
            "withdrawing",
            ended - 86400 * 5,
            ended - 86400,
            Some(ended),
        )
        .await;
    }
    // An active null-sec spawn takes one of the three slots.
    spawn(
        &app,
        (20000050, "Null active"),
        (30000050, "Elsewhere", -0.2),
        "nullsec",
        "established",
        t - 3600,
        t - 3600,
        None,
    )
    .await;
    // Low-sec ended 40 hours ago: overdue.
    spawn(
        &app,
        (20000100, "Low"),
        (30000100, "Lowish", 0.3),
        "lowsec",
        "withdrawing",
        t - 86400 * 6,
        t - 86400 * 2,
        Some(t - 40 * 3600),
    )
    .await;

    let res = app.get("/api/incursions/windows", None).await;
    assert_eq!(res.status, 200);
    let body: Value = res.json();
    assert_eq!(body["opens_after_hours"], 12);
    assert_eq!(body["closes_after_hours"], 36);
    let bands = body["bands"].as_array().unwrap();
    let band = |name: &str| bands.iter().find(|b| b["band"] == name).unwrap();

    let high = band("highsec");
    assert_eq!(
        (high["slots"].as_u64(), high["active"].as_u64()),
        (Some(1), Some(1))
    );
    assert!(high["windows"].as_array().unwrap().is_empty());

    let null = band("nullsec");
    assert_eq!(null["slots"], 3);
    assert_eq!(null["active"], 1);
    assert_eq!(null["free_slots"], 2);
    assert_eq!(null["unknown_windows"], 0);
    let windows = null["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2, "one per free slot, from the latest ends");
    assert_eq!(windows[0]["after_constellation"]["name"], "Null 0");
    assert_eq!(windows[0]["status"], "pending");
    let until_open = windows[0]["seconds_until_open"].as_i64().unwrap();
    assert!(
        (10 * 3600 - 60..=10 * 3600).contains(&until_open),
        "{until_open}"
    );
    assert_eq!(windows[1]["after_constellation"]["name"], "Null 1");
    assert_eq!(windows[1]["status"], "open");
    assert_eq!(windows[1]["seconds_until_open"], 0);

    let low = band("lowsec");
    assert_eq!(low["windows"][0]["status"], "overdue");
    assert_eq!(low["windows"][0]["seconds_until_close"], 0);
}

#[tokio::test]
async fn empty_tracker_answers_cleanly() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let body: Value = app.get("/api/incursions", None).await.json();
    assert_eq!(body["incursions"], json!([]));
    assert!(body["last_poll_at"].is_null());
    let windows: Value = app.get("/api/incursions/windows", None).await.json();
    let null = windows["bands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["band"] == "nullsec")
        .unwrap()
        .clone();
    assert_eq!(null["free_slots"], 3);
    assert_eq!(null["unknown_windows"], 3);
}

#[tokio::test]
async fn anonymous_clients_get_the_incursion_stream() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let request = TestApp::build(Method::GET, "/api/incursions/stream", None)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body().into_data_stream();
    let opened = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("the opening comment")
        .expect("stream open")
        .unwrap();
    assert_eq!(
        String::from_utf8(opened.to_vec()).unwrap(),
        ": connected\n\n"
    );
    for _ in 0..100 {
        if app.live().subscriber_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Only the incursions topic reaches it.
    app.live().publish(
        topics::WAITLIST,
        events::WAITLIST_UPDATE,
        r#"{"waitlist_id":1}"#,
    );
    app.live().publish(
        topics::INCURSIONS,
        events::INCURSIONS_UPDATE,
        r#"{"spawn_ids":[1]}"#,
    );
    let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("an event")
        .expect("stream open")
        .unwrap();
    assert_eq!(
        String::from_utf8(chunk.to_vec()).unwrap(),
        "event: incursions_update\ndata: {\"spawn_ids\":[1]}\n\n"
    );

    // The authenticated stream still requires a session.
    let res = app.get("/api/sse/stream", None).await;
    assert_eq!(res.status, 401);
}
