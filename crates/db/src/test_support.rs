//! Throwaway databases for integration tests.
//!
//! Enable with `wl-db = { workspace = true, features = ["test-support"] }` in a crate's
//! `[dev-dependencies]`, then:
//!
//! ```ignore
//! #[tokio::test]
//! async fn my_test() {
//!     let Some(db) = wl_db::test_support::TestDb::new().await else { return };
//!     sqlx::query("SELECT 1").execute(db.pool()).await.unwrap();
//! }
//! ```
//!
//! Each `TestDb` is a uniquely named database on the server in `TEST_DATABASE_URL` (e.g.
//! `mysql://root:root@127.0.0.1:3307`; any database path in the URL is ignored). It is dropped
//! when the `TestDb` goes out of scope. Without `TEST_DATABASE_URL` the constructors print why and
//! return `None`, so the test passes as skipped.

use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{ConnectOptions, Connection};

use crate::Db;

/// Environment variable naming the MySQL server used by integration tests.
pub const TEST_DATABASE_URL: &str = "TEST_DATABASE_URL";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named, throwaway database. Dropped (best effort) on `Drop`.
pub struct TestDb {
    pool: Db,
    name: String,
    server: MySqlConnectOptions,
}

impl TestDb {
    /// A fresh database with all migrations applied, or `None` if `TEST_DATABASE_URL` is unset.
    ///
    /// # Panics
    /// If the server is unreachable or migrations fail (it is test code: fail loudly).
    pub async fn new() -> Option<Self> {
        let db = Self::empty().await?;
        crate::migrate(db.pool())
            .await
            .unwrap_or_else(|e| panic!("migrating test database {}: {e}", db.name));
        Some(db)
    }

    /// A fresh database without any tables, or `None` if `TEST_DATABASE_URL` is unset.
    pub async fn empty() -> Option<Self> {
        let Ok(url) = std::env::var(TEST_DATABASE_URL) else {
            eprintln!("skipping: {TEST_DATABASE_URL} is not set");
            return None;
        };
        let server = MySqlConnectOptions::from_str(&url)
            .unwrap_or_else(|e| panic!("invalid {TEST_DATABASE_URL}: {e}"))
            .disable_statement_logging();

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let name = format!(
            "wl_test_{}_{}_{}",
            std::process::id(),
            nanos,
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );

        let mut conn = server
            .connect()
            .await
            .unwrap_or_else(|e| panic!("connecting to {TEST_DATABASE_URL}: {e}"));
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE `{name}` CHARACTER SET utf8mb4"
        )))
        .execute(&mut conn)
        .await
        .unwrap_or_else(|e| panic!("creating test database {name}: {e}"));
        let _ = conn.close().await;

        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(server.clone().database(&name))
            .await
            .unwrap_or_else(|e| panic!("connecting to test database {name}: {e}"));

        Some(Self { pool, name, server })
    }

    pub fn pool(&self) -> &Db {
        &self.pool
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // `Drop` cannot await, and we may be inside a runtime; drop the database from a helper
        // thread with its own small runtime.
        let name = std::mem::take(&mut self.name);
        let server = self.server.clone();
        let result = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let mut conn = server.connect().await.map_err(|e| e.to_string())?;
                // Don't hang forever if a leaked connection still holds a lock.
                sqlx::query("SET SESSION lock_wait_timeout = 10")
                    .execute(&mut conn)
                    .await
                    .map_err(|e| e.to_string())?;
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS `{name}`"
                )))
                .execute(&mut conn)
                .await
                .map_err(|e| format!("{name}: {e}"))?;
                let _ = conn.close().await;
                Ok::<_, String>(())
            })
        })
        .join();
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("warning: could not drop test database: {e}"),
            Err(_) => eprintln!("warning: test database cleanup thread panicked"),
        }
    }
}
