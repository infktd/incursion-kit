//! `/api/history/fleet*`, `/api/history/xup` and `/api/stats` (hull names from a small static
//! data set).

mod common;

use std::sync::Arc;

use common::TestApp;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wl_eve_data::{EveData, Tables, TypeRow};

const VINDICATOR: i32 = 17740;
const PALADIN: i32 = 28659;
const UNKNOWN_HULL: i32 = 999_999;
/// 2024-07-15 12:00 UTC: mid-month, so the month is the same in any session time zone.
const JULY: i64 = 1_721_044_800;

fn eve_data() -> Arc<EveData> {
    let ship = |id, name: &str| TypeRow {
        id,
        name: name.to_string(),
        group_id: 900,
        category_id: Some(6),
        published: true,
    };
    Arc::new(EveData::from_tables(
        Tables {
            types: vec![ship(VINDICATOR, "Vindicator"), ship(PALADIN, "Paladin")],
            ..Tables::default()
        },
        None,
    ))
}

async fn app() -> Option<TestApp> {
    TestApp::with_services(|state, _, _| state.with_eve_data(eve_data())).await
}

async fn activity(
    app: &TestApp,
    character_id: i64,
    fleet_id: i64,
    hull: i32,
    first: i64,
    last: i64,
    boss: bool,
) {
    sqlx::query(
        "INSERT INTO fleet_activity (character_id, fleet_id, first_seen, last_seen, hull, has_left, is_boss)
         VALUES (?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(character_id)
    .bind(fleet_id)
    .bind(first)
    .bind(last)
    .bind(hull)
    .bind(boss)
    .execute(app.pool())
    .await
    .unwrap();
}

async fn xup(app: &TestApp, character_id: i64, hull: i32, dna: &str, implants: &str, at: i64) {
    sqlx::query("INSERT IGNORE INTO fitting (dna, hull) VALUES (?, ?)")
        .bind(dna)
        .bind(hull)
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query("INSERT IGNORE INTO implant_set (implants) VALUES (?)")
        .bind(implants)
        .execute(app.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO fit_history (character_id, fit_id, implant_set_id, logged_at)
         SELECT ?, f.id, i.id, ? FROM fitting f, implant_set i WHERE f.dna = ? AND i.implants = ?",
    )
    .bind(character_id)
    .bind(at)
    .bind(dna)
    .bind(implants)
    .execute(app.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn fleet_history_for_self_alt_and_fc() {
    let Some(app) = app().await else { return };
    let main = app.create_account(1001, "Main", None).await;
    app.create_character(1002, "Alt").await;
    app.link_alt(1001, 1002).await;
    let stranger = app
        .create_account(1003, "Stranger", Some("trainee-advanced"))
        .await;
    let fc = app.create_account(1004, "Fc", Some("fc")).await;

    activity(&app, 1002, 1, VINDICATOR, 100, 400, false).await;
    activity(&app, 1002, 2, PALADIN, 1000, 2000, false).await;
    activity(&app, 1002, 3, VINDICATOR, 5000, 5100, false).await;

    let expected = json!({
        "activity": [
            {"hull": {"id": VINDICATOR, "name": "Vindicator"}, "logged_at": 5000, "time_in_fleet": 100},
            {"hull": {"id": PALADIN, "name": "Paladin"}, "logged_at": 1000, "time_in_fleet": 1000},
            {"hull": {"id": VINDICATOR, "name": "Vindicator"}, "logged_at": 100, "time_in_fleet": 300},
        ],
        "summary": [
            {"hull": {"id": PALADIN, "name": "Paladin"}, "time_in_fleet": 1000},
            {"hull": {"id": VINDICATOR, "name": "Vindicator"}, "time_in_fleet": 400},
        ],
    });
    for login in [&main, &fc] {
        let res = app
            .get("/api/history/fleet?character_id=1002", Some(login))
            .await;
        assert_eq!(res.status, 200, "{}", res.text());
        assert_eq!(res.json::<Value>(), expected);
    }
    let res = app
        .get("/api/history/fleet?character_id=1002", Some(&stranger))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));

    // Hull names come from the static data.
    activity(&app, 1002, 4, UNKNOWN_HULL, 6000, 6001, false).await;
    let res = app
        .get("/api/history/fleet?character_id=1002", Some(&main))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "type error: no matches for item")
    );
}

#[tokio::test]
async fn fleet_comp_history() {
    let Some(app) = app().await else { return };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    let trainee = app
        .create_account(1002, "T", Some("trainee-advanced"))
        .await;
    app.create_character(2001, "Pilot A").await;
    app.create_character(2002, "Pilot B").await;

    let res = app.get("/api/history/fleet-comp/latest", Some(&fc)).await;
    let now = chrono::Utc::now().timestamp();
    let time = res.json::<Value>()["time"].as_i64().unwrap();
    assert!((now - time).abs() < 5, "empty history answers now");

    activity(&app, 1001, 10, VINDICATOR, 100, 500, true).await;
    activity(&app, 2001, 10, PALADIN, 200, 300, false).await;
    activity(&app, 2002, 11, VINDICATOR, 150, 900, false).await;
    activity(&app, 2002, 12, VINDICATOR, 1000, 2000, false).await;

    let res = app.get("/api/history/fleet-comp/latest", Some(&fc)).await;
    assert_eq!(res.json::<Value>(), json!({"time": 2000}));

    let res = app.get("/api/history/fleet-comp?time=250", Some(&fc)).await;
    assert_eq!(
        res.json::<Value>(),
        json!({"fleets": {
            "10": [
                {"hull": {"id": VINDICATOR, "name": "Vindicator"}, "character": {"id": 1001, "name": "Fc"},
                 "logged_at": 100, "time_in_fleet": 400, "is_boss": true},
                {"hull": {"id": PALADIN, "name": "Paladin"}, "character": {"id": 2001, "name": "Pilot A"},
                 "logged_at": 200, "time_in_fleet": 100, "is_boss": false},
            ],
            "11": [
                {"hull": {"id": VINDICATOR, "name": "Vindicator"}, "character": {"id": 2002, "name": "Pilot B"},
                 "logged_at": 150, "time_in_fleet": 750, "is_boss": false},
            ],
        }})
    );

    for path in [
        "/api/history/fleet-comp/latest",
        "/api/history/fleet-comp?time=1",
    ] {
        let res = app.get(path, Some(&trainee)).await;
        assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    }
    // Unparseable query value: legacy answered "no route matches" (404).
    let res = app.get("/api/history/fleet-comp?time=abc", Some(&fc)).await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn xup_history() {
    let Some(app) = app().await else { return };
    let main = app.create_account(1001, "Main", None).await;
    let stranger = app.create_account(1002, "Stranger", None).await;
    let fc = app.create_account(1003, "Fc", Some("fc")).await;

    xup(
        &app,
        1001,
        VINDICATOR,
        "17740:2048;1::",
        "33393:33394:",
        100,
    )
    .await;
    xup(&app, 1001, PALADIN, "28659::", "", 200).await;

    let expected = json!({"xups": [
        {"logged_at": 200, "dna": "28659::", "implants": [], "hull": {"id": PALADIN, "name": "Paladin"}},
        {"logged_at": 100, "dna": "17740:2048;1::", "implants": [33393, 33394],
         "hull": {"id": VINDICATOR, "name": "Vindicator"}},
    ]});
    for login in [&main, &fc] {
        let res = app
            .get("/api/history/xup?character_id=1001", Some(login))
            .await;
        assert_eq!(res.json::<Value>(), expected);
    }
    let res = app
        .get("/api/history/xup?character_id=1001", Some(&stranger))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    let res = app.get("/api/history/xup?character_id=1001", None).await;
    assert_eq!(res.status, 401);
}

#[tokio::test]
async fn statistics() {
    let Some(app) = app().await else { return };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    app.create_character(2001, "Pilot A").await;
    app.create_character(2002, "Pilot B").await;

    let empty: Value = app.get("/api/stats", Some(&fc)).await.json();
    assert_eq!(
        empty,
        json!({
            "fleet_seconds_by_hull_by_month": {},
            "xes_by_hull_by_month": {},
            "fleet_seconds_by_month": {},
            "pilots_by_month": {},
            "xes_by_hull_28d": {"Other": 0.0},
            "fleet_seconds_by_hull_28d": {"Other": 0.0},
            // 0/0: NaN, serialized as null (legacy behaviour).
            "x_vs_time_by_hull_28d": {"Other": {"Time": 0.0, "X": null}},
            "time_spent_in_fleet_by_month": {},
        })
    );

    let recent = chrono::Utc::now().timestamp() - 86400;
    activity(&app, 2001, 1, VINDICATOR, JULY, JULY + 7200, false).await;
    activity(&app, 2002, 1, PALADIN, JULY, JULY + 600, false).await;
    activity(&app, 2001, 2, VINDICATOR, recent, recent + 3000, false).await;
    xup(&app, 2001, VINDICATOR, "17740::", "", JULY).await;
    xup(&app, 2002, VINDICATOR, "17740::", "", JULY).await;
    xup(&app, 2002, PALADIN, "28659::", "", recent).await;

    let stats: Value = app.get("/api/stats", Some(&fc)).await.json();
    assert_eq!(
        stats["fleet_seconds_by_hull_by_month"]["2024-07"],
        json!({"Vindicator": 7200.0, "Paladin": 600.0, "Other": 0.0})
    );
    assert_eq!(
        stats["xes_by_hull_by_month"]["2024-07"],
        json!({"Vindicator": 2.0, "Other": 0.0})
    );
    assert_eq!(stats["fleet_seconds_by_month"]["2024-07"], json!(7800.0));
    assert_eq!(stats["pilots_by_month"]["2024-07"], json!(2.0));
    assert_eq!(
        stats["time_spent_in_fleet_by_month"]["2024-07"],
        json!({"a. <1h": 1.0, "b. 1-5h": 1.0})
    );
    assert_eq!(
        stats["xes_by_hull_28d"],
        json!({"Paladin": 1.0, "Other": 0.0})
    );
    assert_eq!(
        stats["fleet_seconds_by_hull_28d"],
        json!({"Vindicator": 3000.0, "Other": 0.0})
    );
    // The Vindicator time has no x-up share of its own, so it lands in "Other".
    assert_eq!(
        stats["x_vs_time_by_hull_28d"],
        json!({"Paladin": {"X": 1.0, "Time": 0.0}, "Other": {"X": 0.0, "Time": 1.0}})
    );

    let trainee = app
        .create_account(1002, "T", Some("trainee-advanced"))
        .await;
    let res = app.get("/api/stats", Some(&trainee)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
}
