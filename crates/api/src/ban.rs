//! Ban lookups (DB table `ban`), shared by the ban routes and the routes that check bans.
//!
//! A ban targets a `Character`, `Corporation`, `Alliance` or `Account`. It is active while
//! `revoked_at` is NULL (permanent) or in the future (`revoked_at` doubles as the expiry time).
//! A character is banned by its own ban, else by its corporation's, else by its alliance's
//! (resolved through the `character` and `corporation` tables).

use wl_core::types::{Ban, Character, Entity};
use wl_db::Db;

#[derive(sqlx::FromRow)]
struct BanRow {
    id: i64,
    entity_id: i64,
    entity_name: Option<String>,
    entity_type: String,
    issued_at: i64,
    public_reason: Option<String>,
    reason: String,
    revoked_at: Option<i64>,
    issued_by_id: i64,
    issued_by_name: String,
    revoked_by_id: Option<i64>,
    revoked_by_name: Option<String>,
}

impl BanRow {
    /// `with_revoker`: whether to report who revoked the ban (history) or always `null` (lists of
    /// active bans), as the legacy endpoints did.
    fn into_ban(self, with_revoker: bool) -> Ban {
        Ban {
            id: Some(self.id),
            entity: Some(Entity {
                id: self.entity_id,
                name: self.entity_name,
                category: self.entity_type,
            }),
            issued_at: Some(self.issued_at),
            issued_by: Some(Character {
                id: self.issued_by_id,
                name: self.issued_by_name,
                corporation_id: None,
            }),
            public_reason: self.public_reason,
            reason: self.reason,
            revoked_at: self.revoked_at,
            revoked_by: self
                .revoked_by_id
                .filter(|_| with_revoker)
                .map(|id| Character {
                    id,
                    name: self.revoked_by_name.unwrap_or_default(),
                    corporation_id: None,
                }),
        }
    }
}

const SELECT_BANS: &str = "SELECT ban.id, entity_id, entity_name, entity_type, issued_at,
        public_reason, reason, revoked_at,
        issuer.id AS issued_by_id, issuer.name AS issued_by_name,
        revoker.id AS revoked_by_id, revoker.name AS revoked_by_name
    FROM ban
    JOIN `character` issuer ON ban.issued_by = issuer.id
    LEFT JOIN `character` revoker ON ban.revoked_by = revoker.id";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Every active ban (any entity), oldest first.
pub async fn all_active_bans(db: &Db) -> Result<Vec<Ban>, sqlx::Error> {
    let rows: Vec<BanRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_BANS} WHERE ban.revoked_at IS NULL OR ban.revoked_at > ? ORDER BY ban.id"
    )))
    .bind(now())
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|row| row.into_ban(false)).collect())
}

/// Active bans on one entity, or `None` if there are none.
pub async fn active_bans(
    db: &Db,
    entity_id: i64,
    entity_type: &str,
) -> Result<Option<Vec<Ban>>, sqlx::Error> {
    let rows: Vec<BanRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_BANS} WHERE entity_id = ? AND entity_type = ?
            AND (ban.revoked_at IS NULL OR ban.revoked_at > ?)
         ORDER BY ban.id"
    )))
    .bind(entity_id)
    .bind(entity_type)
    .bind(now())
    .fetch_all(db)
    .await?;
    Ok(non_empty(
        rows.into_iter().map(|row| row.into_ban(false)).collect(),
    ))
}

/// Every ban (active or not) on one entity, ordered by issue time, or `None` if there are none.
pub async fn all_bans(
    db: &Db,
    entity_id: i64,
    entity_type: &str,
) -> Result<Option<Vec<Ban>>, sqlx::Error> {
    let rows: Vec<BanRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT_BANS} WHERE entity_id = ? AND entity_type = ? ORDER BY issued_at, ban.id"
    )))
    .bind(entity_id)
    .bind(entity_type)
    .fetch_all(db)
    .await?;
    Ok(non_empty(
        rows.into_iter().map(|row| row.into_ban(true)).collect(),
    ))
}

/// Active bans affecting a character: its own, else its corporation's, else its alliance's.
pub async fn character_bans(db: &Db, character_id: i64) -> Result<Option<Vec<Ban>>, sqlx::Error> {
    if let Some(bans) = active_bans(db, character_id, "Character").await? {
        return Ok(Some(bans));
    }
    let corporation_id: Option<Option<i64>> =
        sqlx::query_scalar("SELECT corporation_id FROM `character` WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await?;
    match corporation_id.flatten() {
        Some(corporation_id) => corporation_bans(db, corporation_id).await,
        None => Ok(None),
    }
}

/// Active bans affecting a corporation: its own, else its alliance's.
pub async fn corporation_bans(
    db: &Db,
    corporation_id: i64,
) -> Result<Option<Vec<Ban>>, sqlx::Error> {
    if let Some(bans) = active_bans(db, corporation_id, "Corporation").await? {
        return Ok(Some(bans));
    }
    let alliance_id: Option<Option<i64>> =
        sqlx::query_scalar("SELECT alliance_id FROM corporation WHERE id = ?")
            .bind(corporation_id)
            .fetch_optional(db)
            .await?;
    match alliance_id.flatten() {
        Some(alliance_id) => alliance_bans(db, alliance_id).await,
        None => Ok(None),
    }
}

/// Active bans on an alliance.
pub async fn alliance_bans(db: &Db, alliance_id: i64) -> Result<Option<Vec<Ban>>, sqlx::Error> {
    active_bans(db, alliance_id, "Alliance").await
}

fn non_empty(bans: Vec<Ban>) -> Option<Vec<Ban>> {
    (!bans.is_empty()).then_some(bans)
}
