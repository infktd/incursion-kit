//! Fleet updater: polls every registered fleet (table `fleet`) with its boss's token.
//!
//! Each cycle, per fleet:
//! 1. A fleet whose boss can't read it any more (no token, missing scope, ESI 403/404) is
//!    unregistered: logged, `comp_update` published, and `waitlist_update` / `status_update` for
//!    the waitlist the fleet was linked to. When ESI rejects the boss's access token (401) before
//!    it expired, the token is refreshed once and the read retried; a refresh token EVE rejects
//!    (revoked) or a second 401 unregisters the fleet too, instead of failing every cycle.
//! 2. Members missing from the `character` table are added (names from ESI).
//! 3. Pilots who are now in the fleet are removed from the waitlists (an alt x-up removes only
//!    that character's fits; a main x-up removes the account's non-alt fits), and
//!    `waitlist_update` (and the public `status_update`) is published right away for every
//!    waitlist that changed.
//! 4. `fleet_activity` is updated (only while the fleet has at least `min_in_fleet` members) and
//!    `comp_update` is published when someone joined, left, or changed hull or boss status.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use sqlx::QueryBuilder;
use tokio_util::sync::CancellationToken;
use wl_db::Db;
use wl_esi::fleet::FleetMember;
use wl_esi::{Esi, EsiErrorExt as _};
use wl_live::{Hub, events, topics};

use crate::Result;
use crate::runner::{self, Backoff};

/// Pause between two polls of all fleets.
pub const INTERVAL: Duration = Duration::from_secs(6);
/// A member's `last_seen` is only rewritten when it is older than this many seconds.
const LAST_SEEN_RESOLUTION: i64 = 60;

/// What the fleet updater needs.
#[derive(Clone, Debug)]
pub struct FleetUpdater {
    pub db: Db,
    pub esi: Esi,
    pub live: Hub,
    /// Fleet activity is only tracked while a fleet has at least this many members.
    pub min_in_fleet: usize,
}

/// What one fleet update did (for logs and tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FleetUpdate {
    /// The fleet was unregistered because its boss can't read it (or its token is dead).
    pub unregistered: bool,
    /// Waitlists from which pilots now in fleet were removed.
    pub changed_waitlists: Vec<i64>,
    /// Someone joined, left or changed hull/boss status (a `comp_update` was published).
    pub comp_changed: bool,
}

#[derive(Serialize)]
struct CompUpdate {
    fleet_id: i64,
}

#[derive(sqlx::FromRow)]
struct WaitlistFitRow {
    entry_id: i64,
    waitlist_id: i64,
    character_id: i64,
    is_alt: bool,
}

#[derive(sqlx::FromRow)]
struct ActivityRow {
    id: i64,
    character_id: i64,
    hull: i32,
    is_boss: bool,
    last_seen: i64,
}

impl FleetUpdater {
    /// Poll every [`INTERVAL`] until cancelled (backing off from 30 s to 5 min after failures).
    pub async fn run(self, cancel: CancellationToken) {
        runner::run(
            "fleet_updater",
            cancel,
            INTERVAL,
            Backoff::new(Duration::from_secs(30), Duration::from_secs(300)),
            || self.run_once(),
        )
        .await;
    }

    /// Update every registered fleet once. A failing fleet doesn't stop the others; the last
    /// failure is returned.
    pub async fn run_once(&self) -> Result<()> {
        let fleets: Vec<i64> = sqlx::query_scalar("SELECT id FROM fleet")
            .fetch_all(&self.db)
            .await?;
        let mut result = Ok(());
        for fleet_id in fleets {
            if let Err(e) = self.update_fleet(fleet_id).await {
                tracing::warn!(fleet_id, error = %e, "fleet update failed");
                result = Err(e);
            }
        }
        result
    }

    /// Update one fleet (see the module docs).
    pub async fn update_fleet(&self, fleet_id: i64) -> Result<FleetUpdate> {
        let mut update = FleetUpdate::default();
        let boss_id: Option<i64> = sqlx::query_scalar("SELECT boss_id FROM fleet WHERE id = ?")
            .bind(fleet_id)
            .fetch_optional(&self.db)
            .await?;
        let Some(boss_id) = boss_id else {
            return Ok(update); // closed meanwhile
        };

        let members = match self.read_members(boss_id, fleet_id).await {
            Ok(members) => members,
            Err(e) if e.is_unusable_character() || e.is_unauthorized() => {
                tracing::warn!(fleet_id, boss_id, error = %e, "unregistering fleet the boss can no longer read");
                let waitlist_id = unregister_fleet(&self.db, fleet_id).await?;
                self.publish(
                    topics::FLEET_COMP,
                    events::COMP_UPDATE,
                    &CompUpdate { fleet_id },
                );
                if let Some(waitlist_id) = waitlist_id {
                    self.live.waitlist_changed(waitlist_id);
                }
                update.unregistered = true;
                return Ok(update);
            }
            Err(e) => return Err(e.into()),
        };
        // One entry per character (ESI never repeats one; be safe anyway).
        let members: HashMap<i64, &FleetMember> =
            members.iter().map(|m| (m.character_id, m)).collect();
        let member_ids: Vec<i64> = members.keys().copied().collect();

        self.ensure_characters(&member_ids).await?;

        update.changed_waitlists = remove_from_waitlists(&self.db, &member_ids).await?;
        for &waitlist_id in &update.changed_waitlists {
            self.live.waitlist_changed(waitlist_id);
        }

        let tracked = if members.len() >= self.min_in_fleet {
            members
        } else {
            HashMap::new()
        };
        update.comp_changed = track_activity(&self.db, fleet_id, boss_id, &tracked).await?;
        if update.comp_changed {
            self.publish(
                topics::FLEET_COMP,
                events::COMP_UPDATE,
                &CompUpdate { fleet_id },
            );
        }
        Ok(update)
    }

    /// The fleet's members as its boss sees them. A 401 means ESI rejected an access token we
    /// still thought valid (revoked, or signed by a key the SSO no longer uses): refresh it once
    /// and retry, so a token that merely went stale early recovers and a dead one ends in
    /// `NoToken` (revoked refresh token) or another 401.
    async fn read_members(
        &self,
        boss_id: i64,
        fleet_id: i64,
    ) -> wl_esi::EsiResult<Vec<FleetMember>> {
        match self.esi.fleet_members(boss_id, fleet_id).await {
            Err(e) if e.is_unauthorized() => {
                tracing::info!(fleet_id, boss_id, error = %e, "ESI rejected the boss's access token; refreshing it");
                self.esi.expire_access_token(boss_id).await?;
                self.esi.fleet_members(boss_id, fleet_id).await
            }
            result => result,
        }
    }

    fn publish<T: Serialize>(&self, topic: &str, event: &str, payload: &T) {
        if let Err(e) = self.live.publish_json(topic, event, payload) {
            tracing::error!(topic, event, error = %e, "could not publish live event");
        }
    }

    /// Insert `character` rows for members we have never seen (fleet activity references them).
    async fn ensure_characters(&self, member_ids: &[i64]) -> Result<()> {
        if member_ids.is_empty() {
            return Ok(());
        }
        let mut query = QueryBuilder::new("SELECT id FROM `character` WHERE id IN (");
        let mut ids = query.separated(", ");
        for id in member_ids {
            ids.push_bind(*id);
        }
        query.push(")");
        let known: HashSet<i64> = query
            .build_query_scalar()
            .fetch_all(&self.db)
            .await?
            .into_iter()
            .collect();
        let missing: Vec<i64> = member_ids
            .iter()
            .copied()
            .filter(|id| !known.contains(id))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        // One bulk lookup; ESI fails it whole if any ID is unknown, then go one by one.
        let names = match self.esi.names(&missing).await {
            Ok(names) => names,
            Err(e) => {
                tracing::debug!(error = %e, "bulk name lookup failed; looking up members one by one");
                let mut names = HashMap::new();
                for &id in &missing {
                    names.insert(id, self.esi.character(id).await?.name);
                }
                names
            }
        };
        for id in missing {
            let name = match names.get(&id) {
                Some(name) => name.clone(),
                None => self.esi.character(id).await?.name,
            };
            sqlx::query(
                "INSERT INTO `character` (id, name) VALUES (?, ?) \
                 ON DUPLICATE KEY UPDATE name = VALUES(name)",
            )
            .bind(id)
            .bind(name)
            .execute(&self.db)
            .await?;
        }
        Ok(())
    }
}

/// Forget a fleet registration; returns the waitlist it was linked to.
async fn unregister_fleet(db: &Db, fleet_id: i64) -> Result<Option<i64>> {
    let mut tx = db.begin().await?;
    let waitlist_id: Option<i64> =
        sqlx::query_scalar("SELECT waitlist_id FROM fleet_waitlist WHERE fleet_id = ?")
            .bind(fleet_id)
            .fetch_optional(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM fleet_waitlist WHERE fleet_id = ?")
        .bind(fleet_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM fleet_squad WHERE fleet_id = ?")
        .bind(fleet_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM fleet WHERE id = ?")
        .bind(fleet_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(waitlist_id)
}

/// Remove the x-ups of members now in fleet; returns the waitlists that changed.
async fn remove_from_waitlists(db: &Db, member_ids: &[i64]) -> Result<Vec<i64>> {
    if member_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::new(
        "SELECT waitlist_entry_fit.entry_id, waitlist_entry.waitlist_id, \
         waitlist_entry_fit.character_id, waitlist_entry_fit.is_alt \
         FROM waitlist_entry_fit \
         JOIN waitlist_entry ON waitlist_entry_fit.entry_id = waitlist_entry.id \
         WHERE waitlist_entry_fit.character_id IN (",
    );
    let mut ids = query.separated(", ");
    for id in member_ids {
        ids.push_bind(*id);
    }
    query.push(")");
    let rows: Vec<WaitlistFitRow> = query.build_query_as().fetch_all(db).await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut changed = BTreeSet::new();
    let mut alt_characters = BTreeSet::new();
    let mut main_entries = BTreeSet::new();
    for row in rows {
        changed.insert(row.waitlist_id);
        if row.is_alt {
            alt_characters.insert(row.character_id);
        } else {
            main_entries.insert(row.entry_id);
        }
    }

    let mut tx = db.begin().await?;
    for character_id in alt_characters {
        sqlx::query("DELETE FROM waitlist_entry_fit WHERE character_id = ?")
            .bind(character_id)
            .execute(&mut *tx)
            .await?;
    }
    for entry_id in main_entries {
        sqlx::query("DELETE FROM waitlist_entry_fit WHERE entry_id = ? AND is_alt = 0")
            .bind(entry_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "DELETE FROM waitlist_entry WHERE id NOT IN (SELECT entry_id FROM waitlist_entry_fit)",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(changed.into_iter().collect())
}

/// Update `fleet_activity` from the tracked members; `true` if the composition changed.
async fn track_activity(
    db: &Db,
    fleet_id: i64,
    boss_id: i64,
    members: &HashMap<i64, &FleetMember>,
) -> Result<bool> {
    let now = chrono::Utc::now().timestamp();
    let mut changed = false;
    let mut tx = db.begin().await?;
    let stored: HashMap<i64, ActivityRow> = sqlx::query_as::<_, ActivityRow>(
        "SELECT id, character_id, hull, is_boss, last_seen FROM fleet_activity \
         WHERE fleet_id = ? AND has_left = 0",
    )
    .bind(fleet_id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|row| (row.character_id, row))
    .collect();

    for (&id, member) in members {
        let is_boss = id == boss_id;
        let insert = match stored.get(&id) {
            Some(row) if row.hull == member.ship_type_id && row.is_boss == is_boss => {
                if row.last_seen < now - LAST_SEEN_RESOLUTION {
                    sqlx::query("UPDATE fleet_activity SET last_seen = ? WHERE id = ?")
                        .bind(now)
                        .bind(row.id)
                        .execute(&mut *tx)
                        .await?;
                }
                false
            }
            Some(row) => {
                sqlx::query("UPDATE fleet_activity SET has_left = 1, last_seen = ? WHERE id = ?")
                    .bind(now)
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
                true
            }
            None => true,
        };
        if insert {
            sqlx::query(
                "INSERT INTO fleet_activity \
                 (character_id, fleet_id, first_seen, last_seen, is_boss, hull, has_left) \
                 VALUES (?, ?, ?, ?, ?, ?, 0)",
            )
            .bind(id)
            .bind(fleet_id)
            .bind(now)
            .bind(now)
            .bind(is_boss)
            .bind(member.ship_type_id)
            .execute(&mut *tx)
            .await?;
            changed = true;
        }
    }

    for (id, row) in &stored {
        if !members.contains_key(id) {
            sqlx::query("UPDATE fleet_activity SET has_left = 1 WHERE id = ?")
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            changed = true;
        }
    }
    tx.commit().await?;
    Ok(changed)
}
