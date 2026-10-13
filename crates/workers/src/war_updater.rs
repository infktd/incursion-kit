//! War updater: keeps `active_war`/`war_participant` in sync with ESI's war list, reloads the
//! [`WarCache`] from them, and refreshes the affiliation of everyone currently in a fleet.
//!
//! The first run walks the war list back from the newest war until a whole page is finished
//! (`war_meta.bootstrap_done`). Later runs fetch wars newer than `war_meta.max_war_id_seen` and
//! re-check every stored active war with its ETag.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wl_db::Db;
use wl_esi::war::WarDetail;
use wl_esi::{Esi, EsiErrorExt as _};

use crate::Result;
use crate::affiliation::Affiliation;
use crate::runner::{self, Backoff};
use crate::war::{WarCache, participants};

const META_LIST_ETAG: &str = "wars_list_etag";
const META_MAX_WAR_ID: &str = "max_war_id_seen";
const META_BOOTSTRAP_DONE: &str = "bootstrap_done";
/// Pause between two war requests, to stay gentle on ESI.
const REQUEST_SPACING: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct WarUpdater {
    pub db: Db,
    pub esi: Esi,
    pub cache: WarCache,
    /// Seconds between runs (`war_updater.interval_seconds`).
    pub interval: Duration,
}

/// What fetching one war found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarFetch {
    Active,
    Finished,
    /// Not modified since the stored ETag.
    Unchanged,
    /// ESI no longer knows the war.
    Missing,
}

impl WarUpdater {
    pub async fn run(self, cancel: CancellationToken) {
        if let Err(e) = self.cache.reload(&self.db).await {
            tracing::error!(error = %e, "could not load the war cache");
        }
        runner::run(
            "war_updater",
            cancel,
            self.interval,
            Backoff::new(Duration::from_secs(300), Duration::from_secs(3600)),
            || self.run_once(),
        )
        .await;
    }

    /// One run: refresh wars and the cache, then fleet members' affiliations (which doesn't
    /// depend on the war refresh succeeding).
    pub async fn run_once(&self) -> Result<()> {
        let wars = self.refresh_wars().await;
        match &wars {
            Ok(()) => self.cache.reload(&self.db).await?,
            Err(e) => {
                tracing::error!(error = %e, "war refresh failed; keeping the stored wars");
                // Keep the cache usable with what the database has.
                self.cache.reload(&self.db).await?;
            }
        }
        self.refresh_fleet_affiliations().await?;
        wars
    }

    async fn refresh_wars(&self) -> Result<()> {
        if self.meta(META_BOOTSTRAP_DONE).await?.as_deref() == Some("1") {
            self.refresh_new_and_active_wars().await
        } else {
            self.bootstrap_active_wars().await?;
            self.set_meta(META_BOOTSTRAP_DONE, "1").await
        }
    }

    /// Corporation (and alliance) of every pilot currently in a fleet, so the fleet view's
    /// at-war flags follow corporation changes.
    async fn refresh_fleet_affiliations(&self) -> Result<()> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT character_id FROM fleet_activity WHERE has_left = 0",
        )
        .fetch_all(&self.db)
        .await?;
        if ids.is_empty() {
            return Ok(());
        }
        tracing::debug!(count = ids.len(), "refreshing affiliation of fleet members");
        Affiliation::new(self.esi.clone(), self.db.clone())
            .update_characters_bulk(&ids)
            .await
    }

    /// Walk back from the newest war until a page has no active war left.
    async fn bootstrap_active_wars(&self) -> Result<()> {
        tracing::info!("bootstrapping active wars");
        let mut page = self.newest_wars(None).await?.unwrap_or_default();
        let Some(&max_seen) = page.iter().max() else {
            return Ok(());
        };
        loop {
            let mut all_finished = true;
            for &war_id in &page {
                if matches!(
                    self.fetch_and_store_war(war_id, None).await?,
                    WarFetch::Active | WarFetch::Unchanged
                ) {
                    all_finished = false;
                }
                tokio::time::sleep(REQUEST_SPACING).await;
            }
            if all_finished {
                break;
            }
            let Some(min_id) = page.iter().min().copied().filter(|&id| id > 1) else {
                break;
            };
            page = self.esi.wars_page(min_id).await?;
            if page.is_empty() {
                break;
            }
        }
        self.set_meta(META_MAX_WAR_ID, &max_seen.to_string()).await
    }

    async fn refresh_new_and_active_wars(&self) -> Result<()> {
        let etag = self.meta(META_LIST_ETAG).await?;
        let max_seen: i64 = self
            .meta(META_MAX_WAR_ID)
            .await?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut new_max = max_seen;

        if let Some(newest) = self.newest_wars(etag.as_deref()).await? {
            let mut to_fetch: Vec<i64> =
                newest.iter().copied().filter(|&id| id > max_seen).collect();
            // Page back while pages still hold wars newer than the last one seen.
            let mut cursor = newest.iter().min().copied();
            while let Some(below) = cursor.filter(|&id| id > max_seen + 1) {
                let page = self.esi.wars_page(below).await?;
                let new: Vec<i64> = page.iter().copied().filter(|&id| id > max_seen).collect();
                if new.is_empty() {
                    break;
                }
                to_fetch.extend(new);
                cursor = page.iter().min().copied().filter(|&id| id > 1);
            }
            for war_id in to_fetch {
                self.fetch_and_store_war(war_id, None).await?;
                new_max = new_max.max(war_id);
                tokio::time::sleep(REQUEST_SPACING).await;
            }
        }

        let active: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, etag FROM active_war")
            .fetch_all(&self.db)
            .await?;
        for (war_id, etag) in active {
            self.fetch_and_store_war(war_id, etag.as_deref()).await?;
            tokio::time::sleep(REQUEST_SPACING).await;
        }

        if new_max > max_seen {
            self.set_meta(META_MAX_WAR_ID, &new_max.to_string()).await?;
        }
        Ok(())
    }

    /// The newest war IDs (`None` if unchanged since `etag`), storing the list's new ETag.
    async fn newest_wars(&self, etag: Option<&str>) -> Result<Option<Vec<i64>>> {
        let response = self.esi.wars(etag).await?;
        if let Some(etag) = &response.etag {
            self.set_meta(META_LIST_ETAG, etag).await?;
        }
        Ok(response.data)
    }

    /// Fetch one war and store it if active, or forget it if finished or gone.
    pub async fn fetch_and_store_war(&self, war_id: i64, etag: Option<&str>) -> Result<WarFetch> {
        let response = match self.esi.war(war_id, etag).await {
            Ok(response) => response,
            Err(e) if e.is_not_found() => {
                self.delete_war(war_id).await?;
                return Ok(WarFetch::Missing);
            }
            Err(e) => return Err(e.into()),
        };
        let Some(war) = response.data else {
            return Ok(WarFetch::Unchanged);
        };
        if war.finished.is_some() {
            self.delete_war(war_id).await?;
            return Ok(WarFetch::Finished);
        }
        self.store_war(&war, response.etag.as_deref()).await?;
        Ok(WarFetch::Active)
    }

    async fn store_war(&self, war: &WarDetail, etag: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut tx = self.db.begin().await?;
        sqlx::query(
            "INSERT INTO active_war (id, etag, updated_at) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE etag = VALUES(etag), updated_at = VALUES(updated_at)",
        )
        .bind(war.id)
        .bind(etag)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM war_participant WHERE war_id = ?")
            .bind(war.id)
            .execute(&mut *tx)
            .await?;
        let (corporations, alliances) = participants(war);
        let entities = corporations
            .into_iter()
            .map(|id| (id, "corporation"))
            .chain(alliances.into_iter().map(|id| (id, "alliance")));
        for (entity_id, category) in entities {
            sqlx::query(
                "INSERT IGNORE INTO war_participant (war_id, entity_id, category) VALUES (?, ?, ?)",
            )
            .bind(war.id)
            .bind(entity_id)
            .bind(category)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete_war(&self, war_id: i64) -> Result<()> {
        let mut tx = self.db.begin().await?;
        sqlx::query("DELETE FROM war_participant WHERE war_id = ?")
            .bind(war_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM active_war WHERE id = ?")
            .bind(war_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT meta_value FROM war_meta WHERE meta_key = ?")
                .bind(key)
                .fetch_optional(&self.db)
                .await?,
        )
    }

    async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO war_meta (meta_key, meta_value) VALUES (?, ?) \
             ON DUPLICATE KEY UPDATE meta_value = VALUES(meta_value)",
        )
        .bind(key)
        .bind(value)
        .execute(&self.db)
        .await?;
        Ok(())
    }
}
