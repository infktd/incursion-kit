//! The in-memory set of corporations and alliances at war, used by x-up and the fleet
//! composition view.
//!
//! The war updater ([`crate::war_updater`]) keeps the `active_war`/`war_participant` tables up to
//! date and reloads this cache from them. Until the first reload the cache is empty, so nobody is
//! at war.
//!
//! ```ignore
//! let at_war = state.war_cache().character_is_at_war(&affiliation, db, character_id).await?;
//! let ids_at_war = state.war_cache().character_ids_at_war(db, &member_ids).await?;
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use sqlx::QueryBuilder;
use wl_db::Db;
use wl_esi::war::{WarDetail, WarEntity};

use crate::Result;
use crate::affiliation::Affiliation;

#[derive(Debug, Default)]
struct Participants {
    corporations: HashSet<i64>,
    alliances: HashSet<i64>,
}

/// Corporations and alliances in an active war. Cheap to clone; clones share the data.
#[derive(Clone, Debug, Default)]
pub struct WarCache {
    inner: Arc<RwLock<Participants>>,
}

impl WarCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> RwLockReadGuard<'_, Participants> {
        // A panic while holding the lock can't leave the sets half-written (they are replaced
        // whole), so a poisoned lock is still safe to read.
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the corporation or the alliance is at war.
    pub fn is_at_war(&self, corporation_id: Option<i64>, alliance_id: Option<i64>) -> bool {
        let participants = self.read();
        corporation_id.is_some_and(|id| participants.corporations.contains(&id))
            || alliance_id.is_some_and(|id| participants.alliances.contains(&id))
    }

    /// `(corporations, alliances)` currently at war.
    pub fn counts(&self) -> (usize, usize) {
        let participants = self.read();
        (
            participants.corporations.len(),
            participants.alliances.len(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.counts() == (0, 0)
    }

    /// Replace the whole cache.
    pub fn replace(&self, corporations: HashSet<i64>, alliances: HashSet<i64>) {
        let mut participants = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *participants = Participants {
            corporations,
            alliances,
        };
    }

    /// Reload the cache from the `war_participant` table.
    pub async fn reload(&self, db: &Db) -> Result<()> {
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT DISTINCT entity_id, category FROM war_participant")
                .fetch_all(db)
                .await?;
        let mut corporations = HashSet::new();
        let mut alliances = HashSet::new();
        for (id, category) in rows {
            match category.as_str() {
                "corporation" => corporations.insert(id),
                "alliance" => alliances.insert(id),
                _ => false,
            };
        }
        tracing::info!(
            corporations = corporations.len(),
            alliances = alliances.len(),
            "war cache reloaded"
        );
        self.replace(corporations, alliances);
        Ok(())
    }

    /// Refresh the character's affiliation from ESI, then check whether its corporation or
    /// alliance is at war (the x-up check). An unknown character is not at war.
    pub async fn character_is_at_war(
        &self,
        affiliation: &Affiliation,
        db: &Db,
        character_id: i64,
    ) -> Result<bool> {
        affiliation.update_character(character_id).await?;
        Ok(self
            .character_ids_at_war(db, &[character_id])
            .await?
            .contains(&character_id))
    }

    /// Which of these characters are at war, from their stored affiliation only (no ESI).
    pub async fn character_ids_at_war(
        &self,
        db: &Db,
        character_ids: &[i64],
    ) -> Result<HashSet<i64>> {
        if character_ids.is_empty() || self.is_empty() {
            return Ok(HashSet::new());
        }
        let affiliations = stored_affiliations(db, character_ids).await?;
        Ok(affiliations
            .into_iter()
            .filter(|(_, (corporation, alliance))| self.is_at_war(*corporation, *alliance))
            .map(|(id, _)| id)
            .collect())
    }
}

/// `character id → (corporation id, alliance id)` as stored.
async fn stored_affiliations(
    db: &Db,
    character_ids: &[i64],
) -> Result<HashMap<i64, (Option<i64>, Option<i64>)>> {
    let mut query = QueryBuilder::new(
        "SELECT c.id, c.corporation_id, corp.alliance_id FROM `character` c \
         LEFT JOIN corporation corp ON corp.id = c.corporation_id WHERE c.id IN (",
    );
    let mut ids = query.separated(", ");
    for id in character_ids {
        ids.push_bind(*id);
    }
    query.push(")");
    let rows: Vec<(i64, Option<i64>, Option<i64>)> = query.build_query_as().fetch_all(db).await?;
    Ok(rows
        .into_iter()
        .map(|(id, corporation, alliance)| (id, (corporation, alliance)))
        .collect())
}

/// Corporation and alliance IDs taking part in a war (aggressor, defender and allies).
pub fn participants(war: &WarDetail) -> (HashSet<i64>, HashSet<i64>) {
    let mut corporations = HashSet::new();
    let mut alliances = HashSet::new();
    let entities: Vec<&WarEntity> = [&war.aggressor, &war.defender]
        .into_iter()
        .chain(&war.allies)
        .collect();
    for entity in entities {
        corporations.extend(entity.corporation_id);
        alliances.extend(entity.alliance_id);
    }
    (corporations, alliances)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(corporation_id: Option<i64>, alliance_id: Option<i64>) -> WarEntity {
        WarEntity {
            corporation_id,
            alliance_id,
            isk_destroyed: None,
            ships_killed: None,
        }
    }

    #[test]
    fn cache_checks_corporation_or_alliance() {
        let cache = WarCache::new();
        assert!(cache.is_empty());
        assert!(!cache.is_at_war(Some(1), Some(2)));
        cache.replace(HashSet::from([1]), HashSet::from([20]));
        assert!(cache.is_at_war(Some(1), None));
        assert!(cache.is_at_war(Some(5), Some(20)));
        assert!(!cache.is_at_war(Some(5), Some(21)));
        assert!(!cache.is_at_war(None, None));
        assert_eq!(cache.clone().counts(), (1, 1));
    }

    #[test]
    fn participants_include_allies() {
        let war = WarDetail {
            id: 1,
            aggressor: entity(Some(10), None),
            defender: entity(None, Some(99)),
            allies: vec![entity(Some(11), Some(98))],
            finished: None,
            declared: None,
            started: None,
            retracted: None,
            mutual: false,
            open_for_allies: true,
        };
        let (corporations, alliances) = participants(&war);
        assert_eq!(corporations, HashSet::from([10, 11]));
        assert_eq!(alliances, HashSet::from([98, 99]));
    }
}
