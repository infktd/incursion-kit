//! Fixtures for the ESI-backed and data-backed routes: the real static data, the example data
//! set, a mock ESI/SSO (wiremock) and stored ESI tokens.
//!
//! ```ignore
//! mod common;
//! mod support;
//!
//! let Some(env) = support::Env::new().await else { return };   // needs MySQL and the SDE
//! let me = env.pilot(1001, Some("fc")).await;                 // character + tokens + ESI mocks
//! ```
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::json;
use tokio::sync::OnceCell;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_esi::{Esi, EsiOptions};
use wl_eve_data::{EveData, TypeId};
use wl_fitting::{DataStore, Fitting};
use wl_live::Subscription;

use crate::common::{Login, TestApp};

pub const CLIENT_ID: &str = "test-client";

/// Every scope a full FC login grants.
pub const ALL_SCOPES: [&str; 7] = [
    "publicData",
    "esi-skills.read_skills.v1",
    "esi-clones.read_implants.v1",
    "esi-fleets.read_fleet.v1",
    "esi-fleets.write_fleet.v1",
    "esi-ui.open_window.v1",
    "esi-search.search_structures.v1",
];

/// Corporation every test pilot is in (stored as fresh, so it is never fetched from ESI).
pub const CORPORATION_ID: i64 = 98_000_001;

/// The static data, loaded once per test binary, or `None` (test skipped) without the file.
pub async fn eve() -> Option<Arc<EveData>> {
    static EVE: OnceCell<Option<Arc<EveData>>> = OnceCell::const_new();
    EVE.get_or_init(|| async {
        let path = std::env::var_os("WL_SDE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
            });
        if !path.exists() {
            eprintln!("SKIPPED: static data file not found at {}", path.display());
            return None;
        }
        Some(Arc::new(
            EveData::load(&path).await.expect("static data file loads"),
        ))
    })
    .await
    .clone()
}

/// Writes wl-fitting's bundled example data set into `dir`.
pub fn copy_example_data(dir: &Path) {
    wl_fitting::example_data::seed(dir, true).expect("write example data");
}

pub fn esi_options(server: &MockServer) -> EsiOptions {
    EsiOptions {
        client_id: CLIENT_ID.into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://waitlist.test/auth/cb".into(),
        user_agent: "incursion-kit-tests/1.0".into(),
        esi_url: server.uri(),
        sso_authorize_url: format!("{}/v2/oauth/authorize", server.uri()),
        sso_token_url: format!("{}/v2/oauth/token", server.uri()),
    }
}

/// A test app with ESI (mocked), static data and the example data set.
pub struct Env {
    pub app: TestApp,
    pub esi: MockServer,
    pub eve: Arc<EveData>,
    pub store: Arc<DataStore>,
}

impl std::ops::Deref for Env {
    type Target = TestApp;
    fn deref(&self) -> &TestApp {
        &self.app
    }
}

impl Env {
    pub async fn new() -> Option<Self> {
        let eve = eve().await?;
        let esi = MockServer::start().await;
        let options = esi_options(&esi);
        let mut store = None;
        let app = TestApp::with_services(|state, db, dir| {
            copy_example_data(dir);
            let data = Arc::new(DataStore::open(eve.clone(), dir).expect("example data loads"));
            store = Some(data.clone());
            state
                .with_esi(Esi::with_options(options, db.clone()).expect("esi"))
                .with_eve_data(eve.clone())
                .with_data_store(data)
        })
        .await?;
        sqlx::query(
            "INSERT INTO corporation (id, name, alliance_id, updated_at) VALUES (?, ?, NULL, ?)",
        )
        .bind(CORPORATION_ID)
        .bind("Test Corp")
        .bind(i64::MAX / 2)
        .execute(app.pool())
        .await
        .expect("insert corporation");
        Some(Self {
            app,
            esi,
            eve,
            store: store.expect("store"),
        })
    }

    /// Store a valid access token for `character_id` with `scopes`.
    pub async fn grant_tokens(&self, character_id: i64, scopes: &[&str]) {
        let scopes = scopes.join(" ");
        sqlx::query(
            "REPLACE INTO access_token (character_id, access_token, expires, scopes) VALUES (?, ?, ?, ?)",
        )
        .bind(character_id)
        .bind(format!("access-{character_id}"))
        .bind(chrono::Utc::now().timestamp() + 3600)
        .bind(&scopes)
        .execute(self.pool())
        .await
        .expect("insert access token");
        sqlx::query(
            "REPLACE INTO refresh_token (character_id, refresh_token, scopes) VALUES (?, ?, ?)",
        )
        .bind(character_id)
        .bind(format!("refresh-{character_id}"))
        .bind(&scopes)
        .execute(self.pool())
        .await
        .expect("insert refresh token");
    }

    /// `GET /characters/{id}` (public info, used by affiliation updates).
    pub async fn mock_character(&self, character_id: i64, name: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "achievement_score": 0,
                "birthday": "2015-03-24T11:37:00Z",
                "bloodline_id": 3,
                "corporation_id": CORPORATION_ID,
                "gender": "male",
                "name": name,
                "race_id": 2,
            })))
            .mount(&self.esi)
            .await;
    }

    pub async fn mock_implants(&self, character_id: i64, implants: &[TypeId]) {
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character_id}/implants")))
            .respond_with(ResponseTemplate::new(200).set_body_json(implants))
            .mount(&self.esi)
            .await;
    }

    /// `GET /characters/{id}/skills` answering with `skills` (trained = active level).
    pub async fn mock_skills(&self, character_id: i64, skills: &HashMap<TypeId, i8>) {
        let skills: Vec<_> = skills
            .iter()
            .map(|(id, level)| {
                json!({
                    "skill_id": id,
                    "active_skill_level": level,
                    "trained_skill_level": level,
                    "skillpoints_in_skill": 1000,
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(format!("/characters/{character_id}/skills")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"skills": skills, "total_sp": 1})),
            )
            .mount(&self.esi)
            .await;
    }

    /// Level 5 in every skill any doctrine fit needs.
    pub fn all_doctrine_skills(&self) -> HashMap<TypeId, i8> {
        let mut skills = HashMap::new();
        for doctrine in self.store.get().fits.iter() {
            let fit = &doctrine.fit;
            for type_id in std::iter::once(fit.hull).chain(fit.modules.keys().copied()) {
                for &skill in self.eve.get(type_id).unwrap().skill_requirements.keys() {
                    skills.insert(skill, 5);
                }
            }
        }
        skills
    }

    /// A character with every scope, full skills, no implants, mocked ESI and (optionally) a
    /// role. Named `Pilot <id>` (what the mocked ESI reports).
    pub async fn pilot(&self, character_id: i64, role: Option<&str>) -> Login {
        let name = format!("Pilot {character_id}");
        let login = self.app.create_account(character_id, &name, role).await;
        self.equip(character_id).await;
        login
    }

    /// An alt of `account_id` with mocked ESI.
    pub async fn alt(&self, account_id: i64, character_id: i64) {
        self.app
            .create_character(character_id, &format!("Pilot {character_id}"))
            .await;
        self.app.link_alt(account_id, character_id).await;
        self.equip(character_id).await;
    }

    async fn equip(&self, character_id: i64) {
        sqlx::query("UPDATE `character` SET corporation_id = ? WHERE id = ?")
            .bind(CORPORATION_ID)
            .bind(character_id)
            .execute(self.pool())
            .await
            .expect("set corporation");
        self.grant_tokens(character_id, &ALL_SCOPES).await;
        self.mock_character(character_id, &format!("Pilot {character_id}"))
            .await;
        self.mock_implants(character_id, &[]).await;
        self.mock_skills(character_id, &self.all_doctrine_skills())
            .await;
    }

    /// The example doctrine fit named `name` (e.g. `ARMOR_PALADIN`).
    pub fn doctrine(&self, name: &str) -> Fitting {
        self.store
            .get()
            .fits
            .iter()
            .find(|fit| fit.name == name)
            .unwrap_or_else(|| panic!("no doctrine fit named {name}"))
            .fit
            .clone()
    }

    pub fn dna(&self, name: &str) -> String {
        self.doctrine(name).to_dna(&self.eve).unwrap()
    }

    pub fn type_id(&self, name: &str) -> TypeId {
        self.eve
            .id_of(name)
            .unwrap_or_else(|| panic!("no type {name}"))
    }

    pub async fn open_waitlist(&self) {
        sqlx::query("UPDATE waitlist SET is_open = 1 WHERE id = 1")
            .execute(self.pool())
            .await
            .unwrap();
    }
}

/// The next event of a subscription (fails after 2 s).
pub async fn next_event(sub: &mut Subscription) -> Arc<wl_live::Event> {
    tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timed out waiting for an event")
        .expect("subscription ended")
}

/// Asserts no event is pending.
pub async fn no_event(sub: &mut Subscription) {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), sub.next()).await {
        panic!("unexpected event {event:?}");
    }
}

/// Base64url without padding (for fake JWTs).
pub fn b64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// An unsigned EVE-style access token.
pub fn jwt(character_id: i64, name: &str, scopes: &[&str]) -> String {
    format!(
        "{}.{}.sig",
        b64url(
            json!({"alg": "RS256", "kid": "JWT-Signature-Key"})
                .to_string()
                .as_bytes()
        ),
        b64url(
            json!({
                "sub": format!("CHARACTER:EVE:{character_id}"),
                "name": name,
                "owner": "owner",
                "scp": scopes,
                "aud": [CLIENT_ID, "EVE Online"],
                "iss": "https://login.eveonline.com",
            })
            .to_string()
            .as_bytes()
        )
    )
}

/// The token endpoint answers the next code exchange as `character_id`.
pub async fn mock_code_exchange(server: &MockServer, character_id: i64, scopes: &[&str]) {
    Mock::given(method("POST"))
        .and(path("/v2/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": jwt(character_id, &format!("Pilot {character_id}"), scopes),
            "token_type": "Bearer",
            "expires_in": 1199,
            "refresh_token": format!("refresh-{character_id}"),
        })))
        .mount(server)
        .await;
}
