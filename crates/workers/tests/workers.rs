//! The workers and shared services against a mock ESI (wiremock) and a throwaway MySQL database
//! (`TEST_DATABASE_URL`; tests pass as skipped without it).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_db::test_support::TestDb;
use wl_esi::{Esi, EsiOptions};
use wl_live::{Hub, Subscription, events, topics};
use wl_workers::affiliation::Affiliation;
use wl_workers::fleet_updater::{FleetUpdate, FleetUpdater};
use wl_workers::incursion_updater::IncursionUpdater;
use wl_workers::skill_updater::SkillUpdater;
use wl_workers::war::WarCache;
use wl_workers::war_updater::{WarFetch, WarUpdater};

const FLEET: i64 = 9_000_001;
const BOSS: i64 = 100;

struct Env {
    db: TestDb,
    esi: Esi,
    server: MockServer,
}

impl Env {
    async fn new() -> Option<Self> {
        let db = TestDb::new().await?;
        let server = MockServer::start().await;
        let uri = server.uri();
        let esi = Esi::with_options(
            EsiOptions {
                client_id: "client".into(),
                client_secret: "secret".into(),
                redirect_url: "http://waitlist.test/auth/cb".into(),
                user_agent: "wl-workers tests".into(),
                esi_url: uri.clone(),
                sso_authorize_url: format!("{uri}/v2/oauth/authorize"),
                sso_token_url: format!("{uri}/v2/oauth/token"),
            },
            db.pool().clone(),
        )
        .unwrap();
        Some(Self { db, esi, server })
    }

    fn pool(&self) -> &wl_db::Db {
        self.db.pool()
    }

    async fn exec(&self, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
            .execute(self.pool())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn character(&self, id: i64, name: &str) {
        sqlx::query("INSERT INTO `character` (id, name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(self.pool())
            .await
            .unwrap();
    }

    async fn tokens(&self, character_id: i64, scopes: &str) {
        sqlx::query(
            "INSERT INTO access_token (character_id, access_token, expires, scopes) VALUES (?, ?, ?, ?)",
        )
        .bind(character_id)
        .bind(format!("access-{character_id}"))
        .bind(chrono::Utc::now().timestamp() + 3600)
        .bind(scopes)
        .execute(self.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO refresh_token (character_id, refresh_token, scopes) VALUES (?, ?, ?)",
        )
        .bind(character_id)
        .bind(format!("refresh-{character_id}"))
        .bind(scopes)
        .execute(self.pool())
        .await
        .unwrap();
    }

    async fn get(&self, url: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(self.pool())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }
}

fn member(character_id: i64, ship_type_id: i32) -> Value {
    json!({
        "character_id": character_id, "join_time": "2024-05-01T12:00:00Z", "role": "squad_member",
        "role_name": "Squad Member", "ship_type_id": ship_type_id, "solar_system_id": 30000142,
        "squad_id": 1, "takes_fleet_warp": true, "wing_id": 1,
    })
}

fn corporation(name: &str, alliance_id: Option<i64>) -> Value {
    let mut body = json!({
        "state": "active", "type": "player_owned", "name": name, "ticker": "TICK",
        "description": "", "home_station_id": 60003760, "member_count": 10,
        "tax_rates": {"isk": 10.0, "loyalty_point": 0.0}, "shares": 1000, "war_eligible": true,
        "friendly_fire": "illegal",
    });
    if let Some(alliance_id) = alliance_id {
        body["alliance_id"] = json!(alliance_id);
    }
    body
}

fn alliance(name: &str) -> Value {
    json!({
        "creator_corporation_id": 1, "creator_id": 1, "date_founded": "2010-01-01T00:00:00Z",
        "executor_corporation_id": 1, "name": name, "ticker": "ALLY",
    })
}

async fn next_event(sub: &mut Subscription) -> (String, String, String) {
    let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timed out waiting for an event")
        .expect("subscription ended");
    (event.topic.clone(), event.name.clone(), event.data.clone())
}

async fn no_event(sub: &mut Subscription) {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), sub.next()).await {
        panic!("unexpected event {event:?}");
    }
}

// --- fleet updater ---------------------------------------------------------------------------

/// A waitlist entry for `account_id` with one fit per `(character_id, is_alt)`.
async fn x_up(env: &Env, account_id: i64, fits: &[(i64, bool)]) -> i64 {
    let entry_id = sqlx::query(
        "INSERT INTO waitlist_entry (waitlist_id, account_id, joined_at) VALUES (1, ?, 0)",
    )
    .bind(account_id)
    .execute(env.pool())
    .await
    .unwrap()
    .last_insert_id() as i64;
    for &(character_id, is_alt) in fits {
        sqlx::query(
            "INSERT INTO waitlist_entry_fit (character_id, entry_id, fit_id, implant_set_id, \
             approved, tags, category, cached_time_in_fleet, is_alt) \
             VALUES (?, ?, 1, 1, 0, '', 'other', 0, ?)",
        )
        .bind(character_id)
        .bind(entry_id)
        .bind(is_alt)
        .execute(env.pool())
        .await
        .unwrap();
    }
    entry_id
}

fn fleet_updater(env: &Env, live: &Hub, min_in_fleet: usize) -> FleetUpdater {
    FleetUpdater {
        db: env.pool().clone(),
        esi: env.esi.clone(),
        live: live.clone(),
        min_in_fleet,
    }
}

#[tokio::test]
async fn fleet_updater_removes_joined_pilots_and_tracks_activity() {
    let Some(env) = Env::new().await else { return };
    env.character(BOSS, "Boss").await;
    env.tokens(BOSS, "esi-fleets.read_fleet.v1").await;
    for (id, name) in [
        (201, "Main A"),
        (202, "Alt A"),
        (301, "Main B"),
        (303, "Alt B"),
        (401, "Waiting"),
    ] {
        env.character(id, name).await;
    }
    env.exec(&format!(
        "INSERT INTO fleet (id, boss_id) VALUES ({FLEET}, {BOSS})"
    ))
    .await;
    env.exec("INSERT INTO fitting (id, dna, hull) VALUES (1, '17740::', 17740)")
        .await;
    env.exec("INSERT INTO implant_set (id, implants) VALUES (1, '')")
        .await;
    // A x'd up the main and an alt, B too; the waiting pilot stays out of fleet.
    x_up(&env, 201, &[(201, false), (202, true)]).await;
    x_up(&env, 301, &[(301, false), (303, true)]).await;
    x_up(&env, 401, &[(401, false)]).await;

    // A's main and B's alt join, plus 999 whom the database doesn't know yet.
    let members = Arc::new(std::sync::Mutex::new(json!([
        member(BOSS, 17740),
        member(201, 17740),
        member(303, 17736),
        member(999, 17740),
    ])));
    let current = members.clone();
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .and(header("authorization", "Bearer access-100"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(current.lock().unwrap().clone())
        })
        .mount(&env.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 999, "name": "Newcomer", "category": "character"}
        ])))
        .expect(1)
        .mount(&env.server)
        .await;

    let live = Hub::new();
    let mut sub = live.subscribe(vec![topics::WAITLIST.into(), topics::FLEET_COMP.into()]);
    let updater = fleet_updater(&env, &live, 3);
    let update = updater.update_fleet(FLEET).await.unwrap();
    assert_eq!(
        update,
        FleetUpdate {
            unregistered: false,
            changed_waitlists: vec![1],
            comp_changed: true,
        }
    );
    assert_eq!(
        next_event(&mut sub).await,
        (
            "waitlist".into(),
            events::WAITLIST_UPDATE.into(),
            r#"{"waitlist_id":1}"#.into()
        )
    );
    assert_eq!(
        next_event(&mut sub).await,
        (
            "fleet_comp".into(),
            events::COMP_UPDATE.into(),
            format!(r#"{{"fleet_id":{FLEET}}}"#)
        )
    );
    no_event(&mut sub).await;

    // A's main fit is gone (A's alt fit stays), B's alt fit is gone (B's main fit stays).
    let mut fits: Vec<(i64, bool)> =
        sqlx::query_as("SELECT character_id, is_alt FROM waitlist_entry_fit ORDER BY character_id")
            .fetch_all(env.pool())
            .await
            .unwrap();
    fits.sort();
    assert_eq!(fits, [(202, true), (301, false), (401, false)]);
    assert_eq!(env.scalar("SELECT COUNT(*) FROM waitlist_entry").await, 3);
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM `character` WHERE id = 999 AND name = 'Newcomer'")
            .await,
        1
    );
    let activity: Vec<(i64, i32, bool, bool)> = sqlx::query_as(
        "SELECT character_id, hull, is_boss, has_left FROM fleet_activity ORDER BY character_id",
    )
    .fetch_all(env.pool())
    .await
    .unwrap();
    assert_eq!(
        activity,
        [
            (BOSS, 17740, true, false),
            (201, 17740, false, false),
            (303, 17736, false, false),
            (999, 17740, false, false),
        ]
    );

    // Nothing changed: no events.
    let update = updater.update_fleet(FLEET).await.unwrap();
    assert_eq!(update, FleetUpdate::default());
    no_event(&mut sub).await;

    // 303 switches hull, 201 and 999 leave, A's alt 202 joins.
    *members.lock().unwrap() = json!([member(BOSS, 17740), member(303, 17740), member(202, 17740)]);
    let update = updater.update_fleet(FLEET).await.unwrap();
    assert_eq!(update.changed_waitlists, vec![1]);
    assert!(update.comp_changed);
    assert_eq!(next_event(&mut sub).await.1, events::WAITLIST_UPDATE);
    assert_eq!(next_event(&mut sub).await.1, events::COMP_UPDATE);
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM waitlist_entry WHERE account_id = 201")
            .await,
        0,
        "empty entry removed"
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM fleet_activity WHERE has_left = 0")
            .await,
        3
    );
    assert_eq!(
        env.scalar("SELECT has_left FROM fleet_activity WHERE character_id = 201")
            .await,
        1
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM fleet_activity WHERE character_id = 303")
            .await,
        2
    );

    // Below min_in_fleet everyone is marked as left.
    let update = fleet_updater(&env, &live, 10)
        .update_fleet(FLEET)
        .await
        .unwrap();
    assert!(update.comp_changed);
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM fleet_activity WHERE has_left = 0")
            .await,
        0
    );
}

#[tokio::test]
async fn fleet_updater_unregisters_fleets_the_boss_cannot_read() {
    let Some(env) = Env::new().await else { return };
    env.character(BOSS, "Boss").await;
    env.character(101, "Other Boss").await;
    env.tokens(101, "esi-fleets.read_fleet.v1").await;
    env.exec(&format!(
        "INSERT INTO fleet (id, boss_id) VALUES ({FLEET}, {BOSS})"
    ))
    .await;
    env.exec(&format!(
        "INSERT INTO fleet_squad (fleet_id, category, wing_id, squad_id) VALUES ({FLEET}, 'logi', 1, 1)"
    ))
    .await;
    env.exec("INSERT INTO fleet (id, boss_id) VALUES (5, 101)")
        .await;
    Mock::given(method("GET"))
        .and(path("/fleets/5/members"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not boss"})))
        .mount(&env.server)
        .await;

    let live = Hub::new();
    // BOSS has no token at all; 101 is no longer the boss. Both get unregistered.
    let updater = fleet_updater(&env, &live, 1);
    updater.run_once().await.unwrap();
    assert_eq!(env.scalar("SELECT COUNT(*) FROM fleet").await, 0);
    assert_eq!(env.scalar("SELECT COUNT(*) FROM fleet_squad").await, 0);
    // A fleet that disappeared meanwhile is a no-op.
    assert_eq!(
        updater.update_fleet(FLEET).await.unwrap(),
        FleetUpdate::default()
    );
}

#[tokio::test]
async fn fleet_updater_reports_other_esi_failures() {
    let Some(env) = Env::new().await else { return };
    env.character(BOSS, "Boss").await;
    env.tokens(BOSS, "esi-fleets.read_fleet.v1").await;
    env.exec(&format!(
        "INSERT INTO fleet (id, boss_id) VALUES ({FLEET}, {BOSS})"
    ))
    .await;
    Mock::given(method("GET"))
        .and(path(format!("/fleets/{FLEET}/members")))
        .respond_with(ResponseTemplate::new(502).set_body_json(json!({"error": "bad gateway"})))
        .mount(&env.server)
        .await;
    let updater = fleet_updater(&env, &Hub::new(), 1);
    assert!(updater.run_once().await.is_err());
    assert_eq!(env.scalar("SELECT COUNT(*) FROM fleet").await, 1, "kept");
}

#[tokio::test]
async fn workers_stop_when_cancelled() {
    let Some(env) = Env::new().await else { return };
    let cancel = wl_workers::CancellationToken::new();
    let config = wl_core::Config::from_toml_str(
        r#"
[database]
url = "mysql://unused"
[app]
token_secret = "0000000000000000000000000000000000000000000000000000000000000000"
[esi]
client_id = "id"
client_secret = "secret"
url = "http://waitlist.test/auth/cb"
[skill_updater]
enable = false
[war_updater]
enable = false
[incursion_updater]
enable = false
[incursion_tracker]
enable = false
"#,
    )
    .unwrap();
    let handles = wl_workers::start(
        wl_workers::Services {
            config,
            db: env.pool().clone(),
            esi: env.esi.clone(),
            live: Hub::new(),
            war_cache: WarCache::new(),
            eve_data: None,
        },
        &cancel,
    );
    assert_eq!(handles.len(), 1, "only the fleet updater is enabled");
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("worker stops")
            .expect("worker didn't panic");
    }
}

// --- skills ----------------------------------------------------------------------------------

#[tokio::test]
async fn skills_are_stored_and_changes_logged() {
    let Some(env) = Env::new().await else { return };
    env.character(500, "Skilled").await;
    env.tokens(500, "esi-skills.read_skills.v1").await;
    let skills = Arc::new(std::sync::Mutex::new(json!([
        {"skill_id": 3336, "active_skill_level": 4, "trained_skill_level": 5, "skillpoints_in_skill": 1},
        {"skill_id": 3337, "active_skill_level": 3, "trained_skill_level": 3, "skillpoints_in_skill": 1},
    ])));
    let current = skills.clone();
    Mock::given(method("GET"))
        .and(path("/characters/500/skills"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200)
                .set_body_json(json!({"skills": current.lock().unwrap().clone(), "total_sp": 2}))
        })
        .mount(&env.server)
        .await;

    // First load: stored, no history.
    let active = wl_workers::skills::load_skills(&env.esi, env.pool(), 500)
        .await
        .unwrap();
    assert_eq!(active, HashMap::from([(3336, 4), (3337, 3)]));
    assert_eq!(
        env.scalar("SELECT level FROM skill_current WHERE skill_id = 3336")
            .await,
        5
    );
    assert_eq!(env.scalar("SELECT COUNT(*) FROM skill_history").await, 0);

    // A level up and a new skill are logged.
    *skills.lock().unwrap() = json!([
        {"skill_id": 3336, "active_skill_level": 5, "trained_skill_level": 5, "skillpoints_in_skill": 1},
        {"skill_id": 3337, "active_skill_level": 4, "trained_skill_level": 4, "skillpoints_in_skill": 1},
        {"skill_id": 3338, "active_skill_level": 1, "trained_skill_level": 1, "skillpoints_in_skill": 1},
    ]);
    let updater = SkillUpdater {
        db: env.pool().clone(),
        esi: env.esi.clone(),
        runtime: 0.0,
    };
    updater.run_once().await.unwrap();
    let history: Vec<(i32, i8, i8)> = sqlx::query_as(
        "SELECT skill_id, old_level, new_level FROM skill_history ORDER BY skill_id",
    )
    .fetch_all(env.pool())
    .await
    .unwrap();
    assert_eq!(history, [(3337, 3, 4), (3338, 0, 1)]);

    // A character whose ESI call fails is skipped.
    env.character(501, "No Scope").await;
    env.tokens(501, "publicData").await;
    updater.run_once().await.unwrap();
}

// --- affiliation and wars --------------------------------------------------------------------

#[tokio::test]
async fn affiliation_and_war_cache() {
    let Some(env) = Env::new().await else { return };
    env.get(
        "/characters/600",
        json!({"achievement_score": 0, "birthday": "2015-03-24T11:37:00Z", "bloodline_id": 3, "corporation_id": 70,
               "gender": "male", "name": "Pilot 600", "race_id": 2}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/corporations/70"))
        .respond_with(ResponseTemplate::new(200).set_body_json(corporation("Corp 70", Some(80))))
        .expect(1) // fresh for a day afterwards
        .mount(&env.server)
        .await;
    env.get("/alliances/80", alliance("Alliance 80")).await;

    let affiliation = Affiliation::new(env.esi.clone(), env.pool().clone());
    let cache = WarCache::new();
    assert!(
        !cache
            .character_is_at_war(&affiliation, env.pool(), 600)
            .await
            .unwrap()
    );
    assert_eq!(
        env.scalar("SELECT corporation_id FROM `character` WHERE id = 600")
            .await,
        70
    );
    assert_eq!(
        env.scalar("SELECT alliance_id FROM corporation WHERE id = 70")
            .await,
        80
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM alliance WHERE id = 80")
            .await,
        1
    );

    cache.replace(HashSet::new(), HashSet::from([80]));
    assert!(
        cache
            .character_is_at_war(&affiliation, env.pool(), 600)
            .await
            .unwrap()
    );
    assert_eq!(
        cache
            .character_ids_at_war(env.pool(), &[600, 601])
            .await
            .unwrap(),
        HashSet::from([600])
    );
    assert!(
        cache
            .character_ids_at_war(env.pool(), &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn bulk_affiliation_inserts_unknown_characters() {
    let Some(env) = Env::new().await else { return };
    env.character(700, "Known").await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"character_id": 700, "corporation_id": 71},
            {"character_id": 701, "corporation_id": 71},
        ])))
        .mount(&env.server)
        .await;
    env.get("/corporations/71", corporation("Corp 71", None))
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "unknown id"})))
        .mount(&env.server)
        .await;

    Affiliation::new(env.esi.clone(), env.pool().clone())
        .update_characters_bulk(&[700, 701])
        .await
        .unwrap();
    let rows: Vec<(i64, String, i64)> =
        sqlx::query_as("SELECT id, name, corporation_id FROM `character` ORDER BY id")
            .fetch_all(env.pool())
            .await
            .unwrap();
    assert_eq!(
        rows,
        [(700, "Known".into(), 71), (701, "Character 701".into(), 71)]
    );
}

fn war(id: i64, aggressor_corp: i64, defender_alliance: i64, finished: bool) -> Value {
    let mut war = json!({
        "id": id,
        "aggressor": {"corporation_id": aggressor_corp, "isk_destroyed": 0.0, "ships_killed": 0},
        "defender": {"alliance_id": defender_alliance, "isk_destroyed": 0.0, "ships_killed": 0},
        "allies": [],
        "declared": "2024-05-01T12:00:00Z",
        "mutual": false,
        "open_for_allies": false,
    });
    if finished {
        war["finished"] = json!("2024-06-01T12:00:00Z");
    }
    war
}

#[tokio::test]
async fn war_updater_bootstraps_then_follows_new_and_active_wars() {
    let Some(env) = Env::new().await else { return };
    // First page: 12 active, 11 finished; next page (below 11): 10 finished, 9 finished.
    Mock::given(method("GET"))
        .and(path("/wars"))
        .and(query_param("max_war_id", "11"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([10, 9])))
        .mount(&env.server)
        .await;
    let first_page = Mock::given(method("GET"))
        .and(path("/wars"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"list-1\"")
                .set_body_json(json!([12, 11])),
        )
        .mount_as_scoped(&env.server)
        .await;
    let war_12 = Mock::given(method("GET"))
        .and(path("/wars/12"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"w12\"")
                .set_body_json(war(12, 1000, 2000, false)),
        )
        .mount_as_scoped(&env.server)
        .await;
    for id in [11, 10, 9] {
        env.get(&format!("/wars/{id}"), war(id, 1, 2, true)).await;
    }

    let cache = WarCache::new();
    let updater = WarUpdater {
        db: env.pool().clone(),
        esi: env.esi.clone(),
        cache: cache.clone(),
        interval: Duration::from_secs(3600),
    };
    updater.run_once().await.unwrap();
    assert!(cache.is_at_war(Some(1000), None));
    assert!(cache.is_at_war(None, Some(2000)));
    assert!(!cache.is_at_war(Some(1), Some(2)));
    let meta: Vec<(String, String)> =
        sqlx::query_as("SELECT meta_key, meta_value FROM war_meta ORDER BY meta_key")
            .fetch_all(env.pool())
            .await
            .unwrap();
    assert_eq!(
        meta,
        [
            ("bootstrap_done".into(), "1".into()),
            ("max_war_id_seen".into(), "12".into()),
            ("wars_list_etag".into(), "list-1".into()),
        ]
    );
    drop(first_page);
    drop(war_12);

    // Later: war 13 is new (active), war 12 has finished.
    Mock::given(method("GET"))
        .and(path("/wars"))
        .and(header("if-none-match", "\"list-1\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"list-2\"")
                .set_body_json(json!([13, 12])),
        )
        .mount(&env.server)
        .await;
    env.get("/wars/13", war(13, 3000, 4000, false)).await;
    Mock::given(method("GET"))
        .and(path("/wars/12"))
        .and(header("if-none-match", "\"w12\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(war(12, 1000, 2000, true)))
        .mount(&env.server)
        .await;
    updater.run_once().await.unwrap();
    assert!(!cache.is_at_war(Some(1000), Some(2000)));
    assert!(cache.is_at_war(Some(3000), None));
    assert_eq!(env.scalar("SELECT COUNT(*) FROM active_war").await, 1);
    assert_eq!(
        env.scalar(
            "SELECT CAST(meta_value AS SIGNED) FROM war_meta WHERE meta_key = 'max_war_id_seen'"
        )
        .await,
        13
    );

    // Missing wars are forgotten.
    Mock::given(method("GET"))
        .and(path("/wars/77"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "War not found"})))
        .mount(&env.server)
        .await;
    assert_eq!(
        updater.fetch_and_store_war(77, None).await.unwrap(),
        WarFetch::Missing
    );

    // A fresh cache picks the stored wars up.
    let reloaded = WarCache::new();
    reloaded.reload(env.pool()).await.unwrap();
    assert_eq!(reloaded.counts(), (1, 1));
}

#[tokio::test]
async fn war_updater_refreshes_fleet_members_affiliation() {
    let Some(env) = Env::new().await else { return };
    env.exec("INSERT INTO war_meta (meta_key, meta_value) VALUES ('bootstrap_done', '1')")
        .await;
    Mock::given(method("GET"))
        .and(path("/wars"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&env.server)
        .await;
    env.character(800, "In Fleet").await;
    env.exec(
        "INSERT INTO fleet_activity (character_id, fleet_id, first_seen, last_seen, hull, has_left, is_boss) \
         VALUES (800, 1, 0, 0, 17740, 0, 0)",
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/characters/affiliation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"character_id": 800, "corporation_id": 72},
        ])))
        .expect(1)
        .mount(&env.server)
        .await;
    env.get("/corporations/72", corporation("Corp 72", None))
        .await;
    Mock::given(method("POST"))
        .and(path("/universe/names"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 800, "name": "Renamed", "category": "character"},
        ])))
        .mount(&env.server)
        .await;

    let updater = WarUpdater {
        db: env.pool().clone(),
        esi: env.esi.clone(),
        cache: WarCache::new(),
        interval: Duration::from_secs(3600),
    };
    updater.run_once().await.unwrap();
    let row: (String, i64) =
        sqlx::query_as("SELECT name, corporation_id FROM `character` WHERE id = 800")
            .fetch_one(env.pool())
            .await
            .unwrap();
    assert_eq!(row, ("Renamed".into(), 72));
}

// --- incursions ------------------------------------------------------------------------------

fn incursion(constellation_id: i64, staging: i64) -> Value {
    json!({
        "constellation_id": constellation_id, "faction_id": 500019, "has_boss": false,
        "infested_solar_systems": [staging], "influence": 0.0,
        "staging_solar_system_id": staging, "state": "established", "type": "Incursion",
    })
}

fn system(system_id: i64, security_status: f64) -> Value {
    json!({
        "constellation_id": 1, "name": format!("System {system_id}"),
        "position": {"x": 0.0, "y": 0.0, "z": 0.0}, "security_status": security_status,
        "system_id": system_id,
    })
}

#[tokio::test]
async fn incursion_updater_tracks_the_highsec_focus() {
    let Some(env) = Env::new().await else { return };
    let incursions = Arc::new(std::sync::Mutex::new(json!([
        incursion(20000001, 30000001),
        incursion(20000002, 30000002),
    ])));
    let current = incursions.clone();
    Mock::given(method("GET"))
        .and(path("/incursions"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(current.lock().unwrap().clone())
        })
        .mount(&env.server)
        .await;
    env.get("/universe/systems/30000001", system(30000001, 0.2))
        .await;
    env.get("/universe/systems/30000002", system(30000002, 0.7))
        .await;
    Mock::given(method("GET"))
        .and(path("/universe/constellations/20000002"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "constellation_id": 20000002, "name": "Highsec Place",
            "position": {"x": 0.0, "y": 0.0, "z": 0.0}, "region_id": 10000001, "systems": [30000002],
        })))
        .expect(1) // cached afterwards
        .mount(&env.server)
        .await;

    let updater =
        IncursionUpdater::new(env.pool().clone(), env.esi.clone(), Duration::from_secs(60));
    updater.run_once().await.unwrap();
    let focus = wl_srp::focus::current_focus(env.pool())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(focus.current_focus_constellation_id, Some(20000002));
    assert_eq!(
        focus.current_focus_constellation_name.as_deref(),
        Some("Highsec Place")
    );
    assert!(focus.focus_active);
    assert_eq!(focus.focus_end_timestamp, None);

    updater.run_once().await.unwrap();
    assert_eq!(env.scalar("SELECT COUNT(*) FROM incursion_focus").await, 1);

    // The focus despawns.
    *incursions.lock().unwrap() = json!([incursion(20000001, 30000001)]);
    updater.run_once().await.unwrap();
    let focus = wl_srp::focus::current_focus(env.pool())
        .await
        .unwrap()
        .unwrap();
    assert!(!focus.focus_active);
    assert_eq!(focus.current_focus_constellation_id, None);
    let ended = focus.focus_end_timestamp.expect("end stamped");

    // Still no focus: the end time is kept.
    updater.run_once().await.unwrap();
    let focus = wl_srp::focus::current_focus(env.pool())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(focus.focus_end_timestamp, Some(ended));
}

#[tokio::test]
async fn incursion_updater_fails_on_esi_errors() {
    let Some(env) = Env::new().await else { return };
    Mock::given(method("GET"))
        .and(path("/incursions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&env.server)
        .await;
    let updater =
        IncursionUpdater::new(env.pool().clone(), env.esi.clone(), Duration::from_secs(60));
    assert!(updater.run_once().await.is_err());
    assert_eq!(env.scalar("SELECT COUNT(*) FROM incursion_focus").await, 0);
}
