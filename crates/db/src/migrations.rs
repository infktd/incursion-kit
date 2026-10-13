//! Schema migrations (`crates/db/migrations`, embedded at compile time).
//!
//! `0001_baseline.sql` is byte-for-byte the legacy waitlist schema. A database created by the
//! legacy waitlist therefore already *is* at version 1, but has no `_sqlx_migrations` table.
//! [`adopt`] records version 1 as applied (with the embedded checksum) without running it, then
//! applies any later migrations. [`migrate`] refuses to touch such a database so that nobody
//! adopts a production database by accident.

use sqlx::migrate::Migrator;

use crate::{Db, DbError};

/// All migrations, embedded from `crates/db/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Version of the baseline migration that mirrors the legacy schema.
const BASELINE_VERSION: i64 = 1;

/// Tables every fully migrated legacy database has (including the newest legacy migrations).
const LEGACY_TABLES: &[&str] = &[
    "character",
    "alt_character",
    "admin",
    "access_token",
    "refresh_token",
    "ban",
    "waitlist",
    "waitlist_entry",
    "waitlist_entry_fit",
    "fleet",
    "srp_reports",
    "war_meta",
];

/// Columns added by later legacy migrations; their presence means the legacy schema is current.
const LEGACY_COLUMNS: &[(&str, &str)] = &[
    ("waitlist_entry_fit", "messagexup"),
    ("srp_reports", "discord_message_id"),
    ("character_note", "show_on_waitlist"),
];

/// What the connected database currently contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// No tables at all.
    Empty,
    /// Managed by our migrations (`_sqlx_migrations` exists).
    Managed,
    /// A complete legacy waitlist schema without a migrations table.
    Legacy,
    /// Some tables, but neither of the above. `missing` lists the legacy markers not found.
    Unrecognized { missing: Vec<String> },
}

/// Inspect the current database (the one selected by the pool's URL).
pub async fn schema_state(pool: &Db) -> Result<SchemaState, DbError> {
    if table_exists(pool, &MIGRATOR.table_name).await? {
        return Ok(SchemaState::Managed);
    }

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(pool)
    .await?;
    if table_count == 0 {
        return Ok(SchemaState::Empty);
    }

    let mut missing = Vec::new();
    for table in LEGACY_TABLES {
        if !table_exists(pool, table).await? {
            missing.push(format!("table `{table}`"));
        }
    }
    for (table, column) in LEGACY_COLUMNS {
        if !column_exists(pool, table, column).await? {
            missing.push(format!("column `{table}.{column}`"));
        }
    }

    Ok(if missing.is_empty() {
        SchemaState::Legacy
    } else {
        SchemaState::Unrecognized { missing }
    })
}

/// Apply pending migrations. Fails with [`DbError::LegacyUnadopted`] on a legacy database.
pub async fn migrate(pool: &Db) -> Result<(), DbError> {
    match schema_state(pool).await? {
        SchemaState::Empty | SchemaState::Managed => run(pool).await,
        SchemaState::Legacy => Err(DbError::LegacyUnadopted),
        SchemaState::Unrecognized { missing } => Err(unrecognized(missing)),
    }
}

/// Like [`migrate`], but first adopts a legacy database by recording the baseline as applied.
///
/// Returns `true` if a legacy database was adopted, `false` if there was nothing to adopt (the
/// database was empty or already managed; pending migrations are still applied).
pub async fn adopt(pool: &Db) -> Result<bool, DbError> {
    match schema_state(pool).await? {
        SchemaState::Empty | SchemaState::Managed => {
            run(pool).await?;
            Ok(false)
        }
        SchemaState::Legacy => {
            tracing::info!("adopting legacy waitlist database: recording baseline as applied");
            MIGRATOR.skip(pool, Some(BASELINE_VERSION)).await?;
            run(pool).await?;
            Ok(true)
        }
        SchemaState::Unrecognized { missing } => Err(unrecognized(missing)),
    }
}

async fn run(pool: &Db) -> Result<(), DbError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

fn unrecognized(missing: Vec<String>) -> DbError {
    DbError::UnrecognizedSchema {
        missing: missing.join(", "),
    }
}

async fn table_exists(pool: &Db, table: &str) -> Result<bool, DbError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables \
         WHERE table_schema = DATABASE() AND table_name = ?",
    )
    .bind(table)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

async fn column_exists(pool: &Db, table: &str, column: &str) -> Result<bool, DbError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDb;

    /// Load the baseline the way the legacy waitlist did: plain SQL, no migrations table.
    async fn load_legacy_schema(pool: &Db) {
        let baseline = MIGRATOR
            .iter()
            .find(|m| m.version == BASELINE_VERSION)
            .expect("baseline migration");
        sqlx::raw_sql(baseline.sql.clone())
            .execute(pool)
            .await
            .expect("load legacy schema");
    }

    async fn applied_versions(pool: &Db) -> Vec<(i64, Vec<u8>)> {
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fresh_database_is_migrated() {
        let Some(db) = TestDb::empty().await else {
            return;
        };
        assert_eq!(schema_state(db.pool()).await.unwrap(), SchemaState::Empty);
        migrate(db.pool()).await.unwrap();
        assert_eq!(schema_state(db.pool()).await.unwrap(), SchemaState::Managed);
        // Idempotent.
        migrate(db.pool()).await.unwrap();
        let applied = applied_versions(db.pool()).await;
        assert_eq!(applied.len(), MIGRATOR.iter().count());
        // The baseline seeds the default waitlist.
        let waitlists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waitlist")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(waitlists, 1);
    }

    #[tokio::test]
    async fn legacy_database_requires_adopt_then_adopts() {
        let Some(db) = TestDb::empty().await else {
            return;
        };
        load_legacy_schema(db.pool()).await;
        sqlx::query("INSERT INTO `character` (id, name) VALUES (1, 'Legacy Pilot')")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(schema_state(db.pool()).await.unwrap(), SchemaState::Legacy);

        let err = migrate(db.pool()).await.unwrap_err();
        assert!(matches!(err, DbError::LegacyUnadopted), "{err}");
        assert!(err.to_string().contains("waitlist migrate --adopt"));

        assert!(adopt(db.pool()).await.unwrap());
        assert_eq!(schema_state(db.pool()).await.unwrap(), SchemaState::Managed);
        let applied = applied_versions(db.pool()).await;
        let baseline = MIGRATOR.iter().next().unwrap();
        assert_eq!(applied[0].0, BASELINE_VERSION);
        assert_eq!(applied[0].1, baseline.checksum.to_vec());

        // Data survived, and a later plain `migrate` is happy with the recorded checksum.
        let name: String = sqlx::query_scalar("SELECT name FROM `character` WHERE id = 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(name, "Legacy Pilot");
        migrate(db.pool()).await.unwrap();
        // Adopting again is a no-op.
        assert!(!adopt(db.pool()).await.unwrap());
    }

    #[tokio::test]
    async fn refuses_to_adopt_unknown_schema() {
        let Some(db) = TestDb::empty().await else {
            return;
        };
        sqlx::query("CREATE TABLE `character` (id BIGINT PRIMARY KEY, name VARCHAR(255))")
            .execute(db.pool())
            .await
            .unwrap();
        let err = adopt(db.pool()).await.unwrap_err();
        let DbError::UnrecognizedSchema { missing } = &err else {
            panic!("unexpected error: {err}");
        };
        assert!(missing.contains("alt_character"), "{missing}");
        assert!(!missing.contains("table `character`"), "{missing}");
        assert!(matches!(
            migrate(db.pool()).await.unwrap_err(),
            DbError::UnrecognizedSchema { .. }
        ));
    }

    #[tokio::test]
    async fn refuses_to_adopt_outdated_legacy_schema() {
        let Some(db) = TestDb::empty().await else {
            return;
        };
        load_legacy_schema(db.pool()).await;
        sqlx::query("ALTER TABLE character_note DROP COLUMN show_on_waitlist")
            .execute(db.pool())
            .await
            .unwrap();
        let err = adopt(db.pool()).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("column `character_note.show_on_waitlist`"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn test_db_is_migrated() {
        let Some(db) = TestDb::new().await else {
            return;
        };
        assert_eq!(schema_state(db.pool()).await.unwrap(), SchemaState::Managed);
    }
}
