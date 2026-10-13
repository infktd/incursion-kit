//! Character, corporation and alliance affiliations, refreshed from ESI into the `character`,
//! `corporation` and `alliance` tables.
//!
//! Used by login (`update_character`), the x-up war check (via [`crate::war::WarCache`]), bans
//! and the war updater (`update_characters_bulk` for everyone in a fleet).
//!
//! ```ignore
//! let affiliation = Affiliation::new(esi.clone(), db.clone());
//! affiliation.update_character(character_id).await?;
//! ```

use std::collections::BTreeSet;

use wl_db::Db;
use wl_esi::Esi;

use crate::Result;

/// A corporation fetched within this many seconds is not fetched again.
pub const CORPORATION_MAX_AGE: i64 = 24 * 60 * 60;

/// Most IDs ESI accepts in one bulk request.
const BULK_CHUNK: usize = 1000;

/// Refreshes affiliations from ESI. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Affiliation {
    esi: Esi,
    db: Db,
}

impl Affiliation {
    pub fn new(esi: Esi, db: Db) -> Self {
        Self { esi, db }
    }

    /// Fetch a character's public info, refresh its corporation (and alliance), then store the
    /// character's name and corporation.
    pub async fn update_character(&self, character_id: i64) -> Result<()> {
        let character = self.esi.character(character_id).await?;
        self.update_corporation(character.corporation_id).await?;
        sqlx::query(
            "INSERT INTO `character` (id, name, corporation_id) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE name = VALUES(name), corporation_id = VALUES(corporation_id)",
        )
        .bind(character_id)
        .bind(&character.name)
        .bind(character.corporation_id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Refresh a corporation (and its alliance) unless it was refreshed within
    /// [`CORPORATION_MAX_AGE`].
    pub async fn update_corporation(&self, corporation_id: i64) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let updated_at: Option<i64> =
            sqlx::query_scalar("SELECT updated_at FROM corporation WHERE id = ?")
                .bind(corporation_id)
                .fetch_optional(&self.db)
                .await?;
        if updated_at.is_some_and(|at| at + CORPORATION_MAX_AGE > now) {
            return Ok(());
        }

        let corporation = self.esi.corporation(corporation_id).await?;
        if let Some(alliance_id) = corporation.alliance_id {
            self.update_alliance(alliance_id).await?;
        }
        sqlx::query(
            "INSERT INTO corporation (id, name, alliance_id, updated_at) VALUES (?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE name = VALUES(name), alliance_id = VALUES(alliance_id), \
             updated_at = VALUES(updated_at)",
        )
        .bind(corporation_id)
        .bind(&corporation.name)
        .bind(corporation.alliance_id)
        .bind(now)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Fetch and store an alliance's name.
    pub async fn update_alliance(&self, alliance_id: i64) -> Result<()> {
        let alliance = self.esi.alliance(alliance_id).await?;
        sqlx::query(
            "INSERT INTO alliance (id, name) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE name = VALUES(name)",
        )
        .bind(alliance_id)
        .bind(&alliance.name)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// Refresh the corporation of many characters with the bulk affiliation endpoint, plus their
    /// names when ESI resolves them. Unknown characters are inserted (named `Character <id>` if
    /// ESI can't name them); a known character keeps its name when ESI doesn't return one.
    pub async fn update_characters_bulk(&self, character_ids: &[i64]) -> Result<()> {
        for chunk in character_ids.chunks(BULK_CHUNK) {
            let affiliations = self.esi.character_affiliations(chunk).await?;

            let corporations: BTreeSet<i64> =
                affiliations.iter().map(|a| a.corporation_id).collect();
            for corporation_id in corporations {
                self.update_corporation(corporation_id).await?;
            }

            // ESI fails a whole names request if any ID in it is unknown; names are optional.
            let names = match self.esi.names(chunk).await {
                Ok(names) => names,
                Err(e) => {
                    tracing::debug!(error = %e, "bulk name lookup failed; keeping stored names");
                    Default::default()
                }
            };

            for affiliation in affiliations {
                let id = affiliation.character_id;
                match names.get(&id) {
                    Some(name) => {
                        sqlx::query(
                            "INSERT INTO `character` (id, name, corporation_id) VALUES (?, ?, ?) \
                             ON DUPLICATE KEY UPDATE name = VALUES(name), \
                             corporation_id = VALUES(corporation_id)",
                        )
                        .bind(id)
                        .bind(name)
                        .bind(affiliation.corporation_id)
                        .execute(&self.db)
                        .await?;
                    }
                    None => {
                        sqlx::query(
                            "INSERT INTO `character` (id, name, corporation_id) VALUES (?, ?, ?) \
                             ON DUPLICATE KEY UPDATE corporation_id = VALUES(corporation_id)",
                        )
                        .bind(id)
                        .bind(format!("Character {id}"))
                        .bind(affiliation.corporation_id)
                        .execute(&self.db)
                        .await?;
                    }
                }
            }
        }
        Ok(())
    }
}
