//! End to end: the real waitlist server (API router and fleet updater, over HTTP) against an
//! in-process dev-esi. A pilot and an FC log in through dev-esi's SSO, the pilot x-ups, the FC
//! forms a fleet, registers it and invites the pilot, dev-esi accepts the invite, and the fleet
//! updater takes the pilot off the waitlist and publishes `waitlist_update`.
//!
//! Needs `TEST_DATABASE_URL` and the static data file (`deploy/sde/sde.sqlite` or `WL_SDE_PATH`);
//! skips without them, like the other database and static-data tests.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dev_esi::{DevEsi, Running, SeedSource, Signer, Universe};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wl_api::AppState;
use wl_core::Config;
use wl_db::test_support::TestDb;
use wl_eve_data::EveData;
use wl_fitting::DataStore;
use wl_live::Hub;
use wl_workers::CancellationToken;
use wl_workers::fleet_updater::FleetUpdater;

const CALLBACK: &str = "http://waitlist.test/auth/cb";
const FC: i64 = 2120000011; // Dev FC Brann: `fc` role in the seed, not in a fleet
const PILOT: i64 = 2120000005; // Dev Vindi Tarek: passes the Vindicator fits
const BANNED: i64 = 2120000008; // in a corporation the seed bans

fn sde_path() -> PathBuf {
    std::env::var_os("WL_SDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
        })
}

/// The waitlist server on a real port, its workers, and dev-esi.
struct Stack {
    api: String,
    http: reqwest::Client,
    dev: Running,
    state: AppState,
    cancel: CancellationToken,
    _db: TestDb,
    _data: tempfile::TempDir,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn stack() -> Option<Stack> {
    let sde = sde_path();
    if !sde.exists() {
        eprintln!("skipping: static data file {} not found", sde.display());
        return None;
    }
    let db = TestDb::new().await?;

    let universe = Universe::load(&sde).await.expect("static data for dev-esi");
    let dev = DevEsi::new(
        SeedSource::default_seed(),
        Signer::generate().unwrap(),
        Some(universe),
    )
    .unwrap();
    dev.world().settings.invite_accept_delay_ms = 200;
    let seed = SeedSource::default_seed().parse().unwrap();
    let dev = dev.spawn("127.0.0.1:0").await.unwrap();

    // Roles and bans from the seed, as `deploy/dev.sh --dev-esi` applies them.
    sqlx::raw_sql(sqlx::AssertSqlSafe(dev_esi::seed::waitlist_sql(&seed)))
        .execute(db.pool())
        .await
        .expect("seed SQL");

    let data = tempfile::tempdir().unwrap();
    wl_fitting::example_data::seed(data.path(), true).unwrap();
    let config = Config::from_toml_str(&format!(
        r#"
[database]
url = "mysql://unused"
[app]
token_secret = "{secret}"
[esi]
client_id = "dev-esi-client"
client_secret = "dev-esi-secret"
url = "{CALLBACK}"
esi_url = "{esi}"
sso_authorize_url = "{authorize}"
sso_token_url = "{token}"
[data]
dir = "{dir}"
sde = "{sde}"
"#,
        secret = "11".repeat(32),
        esi = dev.url,
        authorize = dev.authorize_url(),
        token = dev.token_url(),
        dir = data.path().display(),
        sde = sde.display(),
    ))
    .unwrap();

    let eve = Arc::new(EveData::load(&sde).await.unwrap());
    let store = DataStore::open(eve.clone(), data.path()).unwrap();
    let esi = wl_esi::Esi::new(&config, db.pool().clone()).unwrap();
    let live = Hub::new();
    let state = AppState::new(config, db.pool().clone(), live.clone())
        .unwrap()
        .with_eve_data(eve)
        .with_data_store(Arc::new(store))
        .with_esi(esi.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let router = wl_api::router(state.clone());
    tokio::spawn(async move { axum::serve(listener, router).await });

    let cancel = CancellationToken::new();
    let updater = FleetUpdater {
        db: db.pool().clone(),
        esi,
        live,
        min_in_fleet: 1,
    };
    tokio::spawn(updater.run(cancel.child_token()));

    Some(Stack {
        api,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        dev,
        state,
        cancel,
        _db: db,
        _data: data,
    })
}

/// The `name=value` of a response's `Set-Cookie` for `name`.
fn cookie(res: &reqwest::Response, name: &str) -> Option<String> {
    res.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with(&format!("{name}=")))
        .map(|c| c.split(';').next().unwrap().to_string())
}

impl Stack {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api)
    }

    /// The browser's login: login URL, dev-esi's page, pick the character, follow the callback
    /// to the frontend's `/auth/cb`, which posts the code. Returns the response of that post.
    async fn try_login(&self, character_id: i64, query: &str) -> reqwest::Response {
        let res = self
            .http
            .get(self.url(&format!("/api/auth/login_url{query}")))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let nonce = cookie(&res, "wlSsoNonce").expect("nonce cookie");
        let sso_url = res.text().await.unwrap();
        assert!(sso_url.starts_with(&self.dev.authorize_url()), "{sso_url}");

        let page = self.http.get(&sso_url).send().await.unwrap();
        assert!(page.text().await.unwrap().contains("Dev Vindi Tarek"));
        let picked = self
            .http
            .get(format!("{sso_url}&character_id={character_id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(picked.status(), 302);
        let callback = url::Url::parse(picked.headers()["location"].to_str().unwrap()).unwrap();
        assert!(callback.as_str().starts_with(CALLBACK));
        let get = |name: &str| {
            callback
                .query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
                .unwrap()
        };
        self.http
            .post(self.url("/api/auth/cb"))
            .header("cookie", nonce)
            .json(&json!({"code": get("code"), "state": get("state")}))
            .send()
            .await
            .unwrap()
    }

    /// Log in and return the session cookie.
    async fn login(&self, character_id: i64, fc: bool) -> String {
        let res = self
            .try_login(character_id, if fc { "?fc=true" } else { "" })
            .await;
        assert_eq!(res.status(), 200, "{}", res.text().await.unwrap());
        cookie(&res, "authToken").expect("session cookie")
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        session: &str,
        body: Option<Value>,
    ) -> Value {
        let mut request = self
            .http
            .request(method.clone(), self.url(path))
            .header("cookie", session);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let res = request.send().await.unwrap();
        let status = res.status();
        let text = res.text().await.unwrap();
        assert!(status.is_success(), "{method} {path}: {status} {text}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    /// Server-sent events of `session`'s stream, as `(event, data)`.
    async fn events(&self, session: &str) -> mpsc::UnboundedReceiver<(String, String)> {
        let mut res = self
            .http
            .get(self.url("/api/sse/stream"))
            .header("cookie", session)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buffer = String::new();
            while let Ok(Some(chunk)) = res.chunk().await {
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buffer.find("\n\n") {
                    let block: String = buffer.drain(..end + 2).collect();
                    let field = |name: &str| {
                        block
                            .lines()
                            .find_map(|l| l.strip_prefix(&format!("{name}:")))
                            .map(|v| v.trim().to_string())
                    };
                    if let (Some(event), Some(data)) = (field("event"), field("data")) {
                        let _ = tx.send((event, data));
                    }
                }
            }
        });
        rx
    }
}

#[tokio::test]
async fn login_xup_invite_and_the_fleet_updater_sees_the_join() {
    let Some(stack) = stack().await else { return };
    use reqwest::Method;

    // Logins through dev-esi's SSO. The banned corporation's pilot is refused.
    let pilot = stack.login(PILOT, false).await;
    let fc = stack.login(FC, true).await;
    let banned = stack.try_login(BANNED, "").await;
    assert_eq!(banned.status(), 403);
    assert!(
        banned
            .text()
            .await
            .unwrap()
            .contains("Banned from the dev waitlist")
    );

    let whoami = stack.call(Method::GET, "/api/auth/whoami", &fc, None).await;
    assert_eq!(whoami["characters"][0]["name"], "Dev FC Brann");
    assert!(
        whoami["access"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "fleet-invite"),
        "{whoami}"
    );
    let corporation: Option<i64> =
        sqlx::query_scalar("SELECT corporation_id FROM `character` WHERE id = ?")
            .bind(PILOT)
            .fetch_one(stack.state.db())
            .await
            .unwrap();
    assert_eq!(corporation, Some(98800001), "affiliation came from dev-esi");

    // The FC opens the waitlist; the pilot x-ups a doctrine Vindicator (skills and implants are
    // read from dev-esi).
    stack
        .call(
            Method::POST,
            "/api/waitlist/set_open",
            &fc,
            Some(json!({"open": true, "waitlist_id": 1})),
        )
        .await;
    let dna = {
        let data = stack.state.data_store().unwrap().get();
        let fit = data
            .fits
            .iter()
            .find(|f| f.name == "ARMOR_VINDI")
            .expect("example doctrine fit")
            .fit
            .clone();
        fit.to_dna(data.eve()).unwrap()
    };
    let xup = stack
        .call(
            Method::POST,
            "/api/waitlist/xup",
            &pilot,
            Some(json!({
                "waitlist_id": 1, "character_id": PILOT, "eft": "", "is_alt": false,
                "messagexup": "dev-esi e2e", "dna": [{"character_id": PILOT, "dna": dna}],
            })),
        )
        .await;
    assert_eq!(xup["status"], "ok");
    let list = stack
        .call(Method::GET, "/api/waitlist?waitlist_id=1", &fc, None)
        .await;
    let fit = &list["waitlist"][0]["fits"][0];
    assert_eq!(fit["hull"]["name"], "Vindicator");
    assert_eq!(fit["category"], "Other");
    let fit_id = fit["id"].as_i64().unwrap();

    // The FC forms a fleet in game (dev-esi's control API) and registers it with the waitlist.
    let res = stack
        .http
        .post(format!("{}/_dev/fleets", stack.dev.url))
        .json(&json!({"boss_id": FC}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    let info = stack
        .call(
            Method::GET,
            &format!("/api/fleet/info?character_id={FC}"),
            &fc,
            None,
        )
        .await;
    let fleet_id = info["fleet_id"].as_i64().unwrap();
    let squad = |name: &str| {
        info["wings"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|w| {
                w["squads"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["name"] == name)
                    .map(|s| json!([w["id"], s["id"]]))
            })
            .unwrap()
    };
    stack
        .call(
            Method::POST,
            "/api/fleet/register",
            &fc,
            Some(json!({
                "character_id": FC,
                "fleet_id": fleet_id,
                "assignments": {
                    "logi": squad("Logi"), "bastion": squad("Bastion"),
                    "other": squad("Other"), "alt": squad("Alts"),
                },
            })),
        )
        .await;

    // Watch the FC's event stream from here on: the next `waitlist_update` is the updater's.
    let mut events = stack.events(&fc).await;
    let invited = stack
        .call(
            Method::POST,
            "/api/waitlist/invite",
            &fc,
            Some(json!({"id": fit_id, "character_id": FC})),
        )
        .await;
    assert_eq!(invited, Value::String("OK".into()));

    // dev-esi: the pilot accepts and lands in the squad registered for `other`.
    let joined = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some((fleet, member)) = stack.dev.dev.world().fleet_of(PILOT) {
                return (fleet.fleet_id, member.squad_id);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the pilot accepted the invite");
    assert_eq!(joined.0, fleet_id);
    assert_eq!(json!(joined.1), squad("Other")[1]);

    // The fleet updater (polling every 6 s) sees the pilot in fleet, removes the x-up and tells
    // everyone the waitlist changed.
    let update = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match events.recv().await {
                Some((event, data)) if event == "waitlist_update" => return data,
                Some(_) => {}
                None => panic!("event stream ended"),
            }
        }
    })
    .await
    .expect("waitlist_update from the fleet updater");
    assert_eq!(
        serde_json::from_str::<Value>(&update).unwrap(),
        json!({"waitlist_id": 1})
    );
    let list = stack
        .call(Method::GET, "/api/waitlist?waitlist_id=1", &fc, None)
        .await;
    assert_eq!(list["waitlist"], json!([]), "{list}");

    // The waitlist's own fleet view reads the members from dev-esi.
    let members = stack
        .call(
            Method::GET,
            &format!("/api/fleet/members?character_id={FC}"),
            &fc,
            None,
        )
        .await;
    assert!(members.to_string().contains("Dev Vindi Tarek"), "{members}");
}
