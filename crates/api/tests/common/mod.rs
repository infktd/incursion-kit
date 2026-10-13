//! In-process test harness: a router over a throwaway database, plus fixtures.
//!
//! ```ignore
//! mod common;
//! use common::TestApp;
//!
//! #[tokio::test]
//! async fn example() {
//!     let Some(app) = TestApp::new().await else { return };   // skips without TEST_DATABASE_URL
//!     let main = app.create_account(1001, "Main", Some("fc")).await;
//!     let res = app.get("/api/auth/whoami", Some(&main)).await;
//!     assert_eq!(res.status, 200);
//! }
//! ```
#![allow(dead_code)] // each test binary uses a different subset

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use serde::de::DeserializeOwned;
use tower::ServiceExt;
use wl_api::AppState;
use wl_api::auth::Session;
use wl_core::Config;
use wl_db::test_support::TestDb;
use wl_live::Hub;

pub const TOKEN_SECRET: &str = "4b1d7f0c3e5a6b8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f";

/// Test configuration. `https` controls the cookie `Secure` flag.
pub fn test_config(https: bool) -> Config {
    let scheme = if https { "https" } else { "http" };
    Config::from_toml_str(&format!(
        r#"
[database]
url = "mysql://unused"

[app]
token_secret = "{TOKEN_SECRET}"

[esi]
client_id = "test-client"
client_secret = "test-secret"
url = "{scheme}://waitlist.test/auth/cb"
"#
    ))
    .expect("test config")
}

pub struct TestApp {
    pub db: TestDb,
    pub state: AppState,
    pub router: Router,
    /// The content/data directory (`state.content()`), a fresh temporary directory per test.
    pub data_dir: tempfile::TempDir,
}

/// A logged-in account: its id and a ready-made `Cookie` header value.
#[derive(Debug, Clone)]
pub struct Login {
    pub account_id: i64,
    pub cookie: String,
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl TestResponse {
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("utf-8 body")
    }

    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("invalid JSON body ({e}): {}", self.text()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

impl TestApp {
    /// A migrated database and a router over it, or `None` without `TEST_DATABASE_URL`.
    pub async fn new() -> Option<Self> {
        Self::with_config(test_config(false)).await
    }

    pub async fn with_config(config: Config) -> Option<Self> {
        Self::build_with(config, |state, _, _| state).await
    }

    /// Like [`TestApp::new`], with extra services wired into the state (`state.with_esi(..)`...).
    /// The closure also gets the database pool and the data directory, for services that need
    /// them.
    pub async fn with_services(
        wire: impl FnOnce(AppState, &wl_db::Db, &std::path::Path) -> AppState,
    ) -> Option<Self> {
        Self::build_with(test_config(false), wire).await
    }

    async fn build_with(
        config: Config,
        wire: impl FnOnce(AppState, &wl_db::Db, &std::path::Path) -> AppState,
    ) -> Option<Self> {
        let db = TestDb::new().await?;
        let data_dir = tempfile::tempdir().expect("temp data dir");
        let state = AppState::new(config, db.pool().clone(), Hub::new())
            .expect("app state")
            .with_content(wl_content::Content::new(data_dir.path()));
        let state = wire(state, db.pool(), data_dir.path());
        let router = wl_api::router(state.clone());
        Some(Self {
            db,
            state,
            router,
            data_dir,
        })
    }

    pub fn pool(&self) -> &wl_db::Db {
        self.db.pool()
    }

    pub fn live(&self) -> &Hub {
        self.state.live()
    }

    // --- fixtures ---------------------------------------------------------------------------

    pub async fn create_character(&self, id: i64, name: &str) {
        sqlx::query("INSERT INTO `character` (id, name) VALUES (?, ?)")
            .bind(id)
            .bind(name)
            .execute(self.pool())
            .await
            .expect("insert character");
    }

    /// Give `character_id` a role (`trainee`, `fc`, `admin`, ...). The granter is the character
    /// itself, so no extra fixture is needed.
    pub async fn set_role(&self, character_id: i64, role: &str) {
        sqlx::query(
            "REPLACE INTO admin (character_id, role, granted_at, granted_by_id) VALUES (?, ?, 0, ?)",
        )
        .bind(character_id)
        .bind(role)
        .bind(character_id)
        .execute(self.pool())
        .await
        .expect("insert admin role");
    }

    pub async fn link_alt(&self, account_id: i64, alt_id: i64) {
        sqlx::query("REPLACE INTO alt_character (account_id, alt_id) VALUES (?, ?)")
            .bind(account_id)
            .bind(alt_id)
            .execute(self.pool())
            .await
            .expect("insert alt");
    }

    /// Create a main character (optionally with a role) and log it in.
    pub async fn create_account(&self, id: i64, name: &str, role: Option<&str>) -> Login {
        self.create_character(id, name).await;
        if let Some(role) = role {
            self.set_role(id, role).await;
        }
        self.login(id)
    }

    /// A session cookie for `account_id` (no database access).
    pub fn login(&self, account_id: i64) -> Login {
        let token = self
            .state
            .session_key()
            .encode(Session {
                account_id,
                window_character_id: None,
            })
            .expect("encode session");
        Login {
            account_id,
            cookie: format!("authToken={token}"),
        }
    }

    // --- requests ---------------------------------------------------------------------------

    pub async fn request(&self, request: Request<Body>) -> TestResponse {
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        TestResponse {
            status,
            headers,
            body,
        }
    }

    pub fn build(
        method: Method,
        path: &str,
        login: Option<&Login>,
    ) -> axum::http::request::Builder {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(login) = login {
            builder = builder.header(header::COOKIE, &login.cookie);
        }
        builder
    }

    pub async fn get(&self, path: &str, login: Option<&Login>) -> TestResponse {
        self.request(
            Self::build(Method::GET, path, login)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn post_json(
        &self,
        path: &str,
        login: Option<&Login>,
        body: &serde_json::Value,
    ) -> TestResponse {
        self.request(
            Self::build(Method::POST, path, login)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    /// A JSON request with any method (PUT, PATCH...).
    pub async fn send_json(
        &self,
        method: Method,
        path: &str,
        login: Option<&Login>,
        body: &serde_json::Value,
    ) -> TestResponse {
        self.request(
            Self::build(method, path, login)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    /// A request with a raw body (text or bytes) and no content type.
    pub async fn send_raw(
        &self,
        method: Method,
        path: &str,
        login: Option<&Login>,
        body: impl Into<Body>,
    ) -> TestResponse {
        self.request(Self::build(method, path, login).body(body.into()).unwrap())
            .await
    }

    pub async fn delete(&self, path: &str, login: Option<&Login>) -> TestResponse {
        self.request(
            Self::build(Method::DELETE, path, login)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }
}
