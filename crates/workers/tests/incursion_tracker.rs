//! The incursion tracker against a mock ESI (wiremock) and a throwaway MySQL database
//! (`TEST_DATABASE_URL`; tests pass as skipped without it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_db::test_support::TestDb;
use wl_esi::{Esi, EsiOptions};
use wl_eve_data::EveData;
use wl_live::{Hub, Subscription, events, topics};
use wl_workers::incursion_tracker::IncursionTracker;

const HS: i64 = 20000001;
const NS: i64 = 20000002;

struct Env {
    db: TestDb,
    esi: Esi,
    server: MockServer,
    hub: Hub,
    /// What `/incursions` answers, and its `Last-Modified`.
    incursions: Arc<Mutex<(Value, String)>>,
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
        let incursions = Arc::new(Mutex::new((json!([]), last_modified(0))));
        let current = incursions.clone();
        Mock::given(method("GET"))
            .and(path("/incursions"))
            .respond_with(move |_: &wiremock::Request| {
                let (body, modified) = current.lock().unwrap().clone();
                ResponseTemplate::new(200)
                    .insert_header("last-modified", modified.as_str())
                    .set_body_json(body)
            })
            .mount(&server)
            .await;
        Some(Self {
            db,
            esi,
            server,
            hub: Hub::new(),
            incursions,
        })
    }

    fn pool(&self) -> &wl_db::Db {
        self.db.pool()
    }

    /// The next `/incursions` answer, generated `minute` minutes after a fixed time.
    fn set(&self, body: Value, minute: i64) {
        *self.incursions.lock().unwrap() = (body, last_modified(minute));
    }

    async fn mock_map(&self) {
        for (id, name, constellation, security) in [
            (30000001, "Staging HS", HS, 0.9),
            (30000002, "Other HS", HS, 0.8),
            (30000003, "Staging NS", NS, -0.5),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/universe/systems/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "constellation_id": constellation, "name": name,
                    "position": {"x": 0.0, "y": 0.0, "z": 0.0},
                    "security_status": security, "system_id": id,
                })))
                .mount(&self.server)
                .await;
        }
        for (id, name) in [(HS, "Highsec Place"), (NS, "Nullsec Place")] {
            Mock::given(method("GET"))
                .and(path(format!("/universe/constellations/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "constellation_id": id, "name": name,
                    "position": {"x": 0.0, "y": 0.0, "z": 0.0}, "region_id": 10000001,
                    "systems": [],
                })))
                .mount(&self.server)
                .await;
        }
    }

    fn tracker(&self, eve: Option<Arc<EveData>>) -> IncursionTracker {
        IncursionTracker::new(
            self.pool().clone(),
            self.esi.clone(),
            self.hub.clone(),
            eve,
            Duration::from_secs(300),
        )
        .with_focus(true)
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(self.pool())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn events(&self) -> Vec<(i64, String, Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT spawn_id, kind, from_state, to_state FROM incursion_event ORDER BY id",
        )
        .fetch_all(self.pool())
        .await
        .unwrap()
    }
}

fn last_modified(minute: i64) -> String {
    let at = chrono::DateTime::from_timestamp(1_791_640_800 + minute * 60, 0).unwrap();
    at.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn incursion(constellation_id: i64, staging: i64, infested: &[i64], state: &str) -> Value {
    json!({
        "constellation_id": constellation_id, "faction_id": 500019, "has_boss": false,
        "infested_solar_systems": infested, "influence": 0.0,
        "staging_solar_system_id": staging, "state": state, "type": "Incursion",
    })
}

async fn next_update(sub: &mut Subscription) -> Value {
    let event = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timed out waiting for an incursions update")
        .expect("subscription ended");
    assert_eq!(event.name, events::INCURSIONS_UPDATE);
    serde_json::from_str(&event.data).unwrap()
}

async fn no_update(sub: &mut Subscription) {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), sub.next()).await {
        panic!("unexpected event {event:?}");
    }
}

fn kinds(update: &Value) -> Vec<&str> {
    update["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn tracks_spawns_transitions_and_ends() {
    let Some(env) = Env::new().await else { return };
    env.mock_map().await;
    let tracker = env.tracker(None);
    let mut sub = env.hub.subscribe(vec![topics::INCURSIONS.to_owned()]);

    // Two new spawns.
    env.set(
        json!([
            incursion(NS, 30000003, &[30000003], "established"),
            incursion(HS, 30000001, &[30000001, 30000002], "established"),
        ]),
        0,
    );
    let delay = tracker.run_once().await.unwrap();
    assert_eq!(
        delay,
        Duration::from_secs(300),
        "no Expires: the configured interval"
    );
    let update = next_update(&mut sub).await;
    assert_eq!(update["spawn_ids"], json!([1, 2]));
    assert_eq!(kinds(&update), ["spawned", "spawned"]);

    let (band, name, region, observed): (String, String, Option<i64>, i8) = sqlx::query_as(
        "SELECT security_band, constellation_name, region_id, observed_from_start \
         FROM incursion_spawn WHERE constellation_id = ?",
    )
    .bind(HS)
    .fetch_one(env.pool())
    .await
    .unwrap();
    assert_eq!((band.as_str(), name.as_str()), ("highsec", "Highsec Place"));
    assert_eq!(region, Some(10000001));
    assert_eq!(
        observed, 0,
        "the first poll can't know when spawns appeared"
    );
    let roles: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT system_id, role, role_source FROM incursion_spawn_system WHERE spawn_id = 2 \
         ORDER BY system_id",
    )
    .fetch_all(env.pool())
    .await
    .unwrap();
    assert_eq!(
        roles,
        [
            (30000001, "staging".into(), "known".into()),
            (30000002, "unknown".into(), "unknown".into())
        ]
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_influence").await,
        2
    );
    let focus = wl_srp::focus::current_focus(env.pool())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(focus.current_focus_constellation_id, Some(HS));
    assert_eq!(
        focus.current_focus_constellation_name.as_deref(),
        Some("Highsec Place")
    );
    assert!(focus.focus_active);

    // The high-sec spawn mobilizes, its boss appears, influence rises.
    let mut hs = incursion(HS, 30000001, &[30000001, 30000002], "mobilizing");
    hs["has_boss"] = json!(true);
    hs["influence"] = json!(0.5);
    env.set(
        json!([incursion(NS, 30000003, &[30000003], "established"), hs]),
        5,
    );
    tracker.run_once().await.unwrap();
    let update = next_update(&mut sub).await;
    assert_eq!(kinds(&update), ["state_changed", "boss_spawned"]);
    assert_eq!(update["changes"][0]["from_state"], "established");
    assert_eq!(update["changes"][0]["to_state"], "mobilizing");
    assert_eq!(update["influence_changed"], json!([2]));
    assert_eq!(
        env.scalar(
            "SELECT COUNT(*) FROM incursion_spawn WHERE id = 2 AND state = 'mobilizing' \
             AND mobilizing_at IS NOT NULL AND boss_spawned_at IS NOT NULL AND has_boss = 1"
        )
        .await,
        1
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_influence WHERE spawn_id = 2")
            .await,
        2
    );

    // Same ESI data again (one cache period): nothing new, no event.
    tracker.run_once().await.unwrap();
    no_update(&mut sub).await;
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_influence").await,
        4
    );

    // The high-sec spawn despawns: it ends and so does the focus.
    env.set(
        json!([incursion(NS, 30000003, &[30000003], "established")]),
        10,
    );
    tracker.run_once().await.unwrap();
    let update = next_update(&mut sub).await;
    assert_eq!(kinds(&update), ["ended"]);
    assert_eq!(update["changes"][0]["from_state"], "mobilizing");
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_spawn WHERE ended_at IS NOT NULL")
            .await,
        1
    );
    let focus = wl_srp::focus::current_focus(env.pool())
        .await
        .unwrap()
        .unwrap();
    assert!(!focus.focus_active);
    assert!(focus.focus_end_timestamp.is_some());

    // A new spawn in the same constellation is a new spawn, seen from its start.
    env.set(
        json!([
            incursion(NS, 30000003, &[30000003], "established"),
            incursion(HS, 30000001, &[30000001], "established"),
        ]),
        15,
    );
    tracker.run_once().await.unwrap();
    let update = next_update(&mut sub).await;
    assert_eq!(kinds(&update), ["spawned"]);
    assert_eq!(update["spawn_ids"], json!([3]));
    assert_eq!(
        env.scalar("SELECT observed_from_start FROM incursion_spawn WHERE id = 3")
            .await,
        1
    );
    assert_eq!(
        env.events()
            .await
            .into_iter()
            .map(|(id, kind, _, _)| format!("{id}:{kind}"))
            .collect::<Vec<_>>(),
        [
            "1:spawned",
            "2:spawned",
            "2:state_changed",
            "2:boss_spawned",
            "2:ended",
            "3:spawned"
        ]
    );
}

#[tokio::test]
async fn one_empty_list_is_not_trusted() {
    let Some(env) = Env::new().await else { return };
    env.mock_map().await;
    let tracker = env.tracker(None);
    env.set(
        json!([incursion(NS, 30000003, &[30000003], "established")]),
        0,
    );
    tracker.run_once().await.unwrap();

    env.set(json!([]), 5);
    tracker.run_once().await.unwrap();
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_spawn WHERE ended_at IS NULL")
            .await,
        1,
        "a single empty answer ends nothing"
    );
    tracker.run_once().await.unwrap();
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM incursion_spawn WHERE ended_at IS NULL")
            .await,
        0
    );
}

#[tokio::test]
async fn fails_on_esi_errors_without_writing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/incursions"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
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
    let tracker = IncursionTracker::new(
        db.pool().clone(),
        esi,
        Hub::new(),
        None,
        Duration::from_secs(60),
    );
    assert!(tracker.run_once().await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM incursion_tracker_state")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn polls_when_the_esi_cache_expires() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let server = MockServer::start().await;
    let expires = chrono::Utc::now() + chrono::Duration::seconds(120);
    Mock::given(method("GET"))
        .and(path("/incursions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "expires",
                    expires
                        .format("%a, %d %b %Y %H:%M:%S GMT")
                        .to_string()
                        .as_str(),
                )
                .set_body_json(json!([])),
        )
        .mount(&server)
        .await;
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
    let tracker = IncursionTracker::new(
        db.pool().clone(),
        esi,
        Hub::new(),
        None,
        Duration::from_secs(300),
    );
    let delay = tracker.run_once().await.unwrap();
    assert!(
        delay > Duration::from_secs(110) && delay <= Duration::from_secs(126),
        "{delay:?}"
    );
}

/// With the real static data: names, security and the known layout come from the star map,
/// without any ESI lookups (none are mocked).
#[tokio::test]
async fn uses_the_star_map_when_available() {
    let sde = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite");
    if !sde.exists() {
        eprintln!("SKIPPED: no static data file");
        return;
    }
    let eve = Arc::new(EveData::load(&sde).await.unwrap());
    if eve.universe().is_empty() {
        eprintln!("SKIPPED: the static data file has no star map");
        return;
    }
    let Some(env) = Env::new().await else { return };
    let layout = &wl_incursions::roles::known_layouts()[&20000396];
    let mut infested = vec![layout.staging];
    infested.extend(&layout.vanguard);
    infested.extend(&layout.assault);
    infested.extend(&layout.headquarters);
    env.set(
        json!([incursion(
            20000396,
            layout.staging,
            &infested,
            "established"
        )]),
        0,
    );
    env.tracker(Some(eve)).run_once().await.unwrap();
    let (name, region, band): (String, Option<String>, String) = sqlx::query_as(
        "SELECT staging_system_name, region_name, security_band FROM incursion_spawn",
    )
    .fetch_one(env.pool())
    .await
    .unwrap();
    assert_eq!(name, "Claysson");
    assert_eq!(region.as_deref(), Some("Sinq Laison"));
    assert_eq!(band, "highsec");
    assert_eq!(
        env.scalar(
            "SELECT COUNT(*) FROM incursion_spawn_system WHERE role_source = 'known' \
             AND role = 'headquarters'"
        )
        .await,
        1
    );
}
