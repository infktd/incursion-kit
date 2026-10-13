//! Database pool, migrations and test support (MySQL).
//!
//! - [`connect`] builds the pool from [`DatabaseConfig`].
//! - [`migrate`] applies pending migrations; [`adopt`] takes over a database created by the legacy
//!   waitlist (see [`migrations`]).
//! - With the `test-support` feature, [`test_support::TestDb`] gives integration tests a throwaway,
//!   fully migrated database.
//!
//! Queries elsewhere in the workspace use runtime-checked `sqlx::query`/`query_as` with `FromRow`
//! (no compile-time macros, so building never needs a database).

pub mod migrations;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::time::Duration;

use sqlx::mysql::MySqlPoolOptions;
use wl_core::config::DatabaseConfig;

pub use migrations::{MIGRATOR, SchemaState, adopt, migrate, schema_state};

/// The connection pool type used throughout the workspace.
pub type Db = sqlx::MySqlPool;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("could not connect to the database: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(
        "this database was created by the legacy waitlist and is not managed by migrations yet; \
         back it up, then run `waitlist migrate --adopt` once to adopt it"
    )]
    LegacyUnadopted,
    #[error(
        "the database has tables but is neither managed by migrations nor a complete legacy \
         waitlist schema (missing: {missing}); refusing to touch it. If it is a legacy database, \
         apply all of its own SQL migrations first"
    )]
    UnrecognizedSchema { missing: String },
}

/// Open a MySQL pool with the configured limits and timeouts.
pub async fn connect(config: &DatabaseConfig) -> Result<Db, DbError> {
    MySqlPoolOptions::new()
        .min_connections(config.min_connections)
        .max_connections(config.max_connections)
        .idle_timeout(Duration::from_secs(config.idle_timeout))
        .acquire_timeout(Duration::from_secs(config.connect_timeout))
        .connect(&config.url)
        .await
        .map_err(DbError::Connect)
}
