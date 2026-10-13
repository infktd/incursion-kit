//! Applying an ESI `/incursions` snapshot to the stored spawns: the tracker's diffing.
//!
//! An incursion is matched to the active spawn of its constellation. New ones are stored with
//! their names, security, band and system roles; changes of state, boss, staging system and
//! infested systems are recorded as events; spawns missing from the snapshot are ended. Each
//! ESI refresh adds one influence sample per spawn, stamped with ESI's `Last-Modified` time, so
//! the samples are 5 minutes apart and polling twice inside one cache period adds nothing.

use std::collections::{HashMap, HashSet};

use wl_db::Db;
use wl_esi::Esi;
use wl_esi::universe::{Incursion, IncursionsSnapshot};
use wl_eve_data::{SecurityBand, Universe};

use crate::roles::{self, Role, RoleSource};
use crate::store::{self, SpawnRow, kinds};

/// A gap between polls longer than this means spawns first seen afterwards may have spawned
/// during the gap (`observed_from_start` = false).
pub const MAX_POLL_GAP: i64 = 20 * 60;

/// Security status from which the legacy focus counts a staging system as high-sec (unrounded,
/// as the legacy updater compared it).
pub const FOCUS_HIGHSEC: f64 = 0.5;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("ESI error: {0}")]
    Esi(#[from] wl_esi::EsiError),
}

/// One recorded change, with the spawn as stored afterwards.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub kind: &'static str,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub spawn: SpawnRow,
}

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncOutcome {
    pub changes: Vec<Change>,
    /// Spawns whose influence value changed.
    pub influence_changed: Vec<i64>,
    /// The high-sec focus as the legacy updater detects it: the first incursion (in ESI's order)
    /// whose staging system has a security status of at least 0.5. `(constellation id, name)`.
    pub focus: Option<(i64, String)>,
    /// The snapshot was empty while spawns were active and `trust_empty` was false: nothing was
    /// ended.
    pub ignored_empty: bool,
}

impl SyncOutcome {
    /// Whether clients should refetch.
    pub fn changed(&self) -> bool {
        !self.changes.is_empty() || !self.influence_changed.is_empty()
    }

    /// Ids of every spawn that changed, ascending.
    pub fn spawn_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .changes
            .iter()
            .map(|c| c.spawn.id)
            .chain(self.influence_changed.iter().copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// Names and security of a system.
#[derive(Debug, Clone)]
struct SystemInfo {
    name: String,
    security: f64,
}

/// What a new spawn needs that the snapshot doesn't carry.
#[derive(Debug, Clone)]
struct Place {
    constellation_name: String,
    region_id: Option<i64>,
    region_name: Option<String>,
}

/// Looks names up in the star map, falling back to ESI (results kept for the sync).
struct Resolver<'a> {
    universe: &'a Universe,
    esi: &'a Esi,
    systems: HashMap<i64, SystemInfo>,
    places: HashMap<i64, Place>,
}

impl<'a> Resolver<'a> {
    fn new(universe: &'a Universe, esi: &'a Esi) -> Self {
        Self {
            universe,
            esi,
            systems: HashMap::new(),
            places: HashMap::new(),
        }
    }

    async fn system(&mut self, id: i64) -> Result<SystemInfo, SyncError> {
        if let Some(info) = self.systems.get(&id) {
            return Ok(info.clone());
        }
        let info = match self.universe.system(id) {
            Some(s) => SystemInfo {
                name: s.name.clone(),
                security: s.security,
            },
            None => {
                let s = self.esi.solar_system(id).await?;
                SystemInfo {
                    name: s.name,
                    security: s.security_status,
                }
            }
        };
        self.systems.insert(id, info.clone());
        Ok(info)
    }

    async fn place(&mut self, constellation_id: i64) -> Result<Place, SyncError> {
        if let Some(place) = self.places.get(&constellation_id) {
            return Ok(place.clone());
        }
        let place = match self.universe.constellation(constellation_id) {
            Some(c) => Place {
                constellation_name: c.name.clone(),
                region_id: Some(c.region_id),
                region_name: self.universe.region(c.region_id).map(|r| r.name.clone()),
            },
            None => {
                let c = self.esi.constellation(constellation_id).await?;
                Place {
                    constellation_name: c.name,
                    region_id: Some(c.region_id),
                    region_name: None,
                }
            }
        };
        self.places.insert(constellation_id, place.clone());
        Ok(place)
    }
}

/// The infested systems of an incursion, with the staging system.
fn infested(incursion: &Incursion) -> Vec<i64> {
    let mut systems = incursion.infested_solar_systems.clone();
    if !systems.contains(&incursion.staging_solar_system_id) {
        systems.push(incursion.staging_solar_system_id);
    }
    systems
}

/// A system row to store: `(id, name, security, role, source)`.
type SystemEntry = (i64, String, f64, Role, RoleSource);

/// Everything about an incursion's systems, resolved before the transaction starts.
struct Resolved {
    staging: SystemInfo,
    systems: Vec<SystemEntry>,
    place: Option<Place>,
}

async fn resolve(
    resolver: &mut Resolver<'_>,
    incursion: &Incursion,
    new: bool,
) -> Result<Resolved, SyncError> {
    let staging = resolver.system(incursion.staging_solar_system_id).await?;
    let mut systems = Vec::new();
    for (id, role, source) in roles::assign(
        incursion.constellation_id,
        incursion.staging_solar_system_id,
        &infested(incursion),
        resolver.universe,
    ) {
        let info = resolver.system(id).await?;
        systems.push((id, info.name, info.security, role, source));
    }
    let place = if new {
        Some(resolver.place(incursion.constellation_id).await?)
    } else {
        None
    };
    Ok(Resolved {
        staging,
        systems,
        place,
    })
}

/// Applies `snapshot` (fetched at `now`, Unix seconds) to the database.
///
/// `trust_empty`: whether an empty snapshot may end every active spawn. ESI occasionally
/// answers with an empty list; the tracker only trusts it when it repeats.
pub async fn apply(
    db: &Db,
    esi: &Esi,
    universe: &Universe,
    snapshot: &IncursionsSnapshot,
    now: i64,
    trust_empty: bool,
) -> Result<SyncOutcome, SyncError> {
    let mut outcome = SyncOutcome::default();
    let mut changes: Vec<Pending> = Vec::new();
    let active = store::active_spawns(db).await?;
    let by_constellation: HashMap<i64, &SpawnRow> =
        active.iter().map(|s| (s.constellation_id, s)).collect();

    // Network lookups first, so the transaction stays short.
    let mut resolver = Resolver::new(universe, esi);
    let mut resolved = Vec::with_capacity(snapshot.incursions.len());
    for incursion in &snapshot.incursions {
        let existing = by_constellation.get(&incursion.constellation_id).copied();
        let needs_systems = match existing {
            None => true,
            Some(row) => row.staging_system_id != incursion.staging_solar_system_id,
        };
        let systems_changed = match existing {
            Some(row) => !needs_systems && stored_systems_differ(db, row.id, incursion).await?,
            None => false,
        };
        let details = if needs_systems || systems_changed {
            Some(resolve(&mut resolver, incursion, existing.is_none()).await?)
        } else {
            None
        };
        resolved.push((incursion, existing, details));
    }

    let previous = store::tracker_state(db).await?;
    let observed_from_start = previous.is_some_and(|p| now - p.last_poll_at <= MAX_POLL_GAP);
    let sampled_at = snapshot.last_modified.map_or(now, |t| t.timestamp());

    let mut tx = db.begin().await?;
    let mut touched = HashSet::new();
    for (incursion, existing, details) in resolved {
        let id = match existing {
            None => {
                let details = details.expect("resolved for new spawns");
                let id =
                    insert_spawn(&mut tx, incursion, &details, now, observed_from_start).await?;
                replace_systems(&mut tx, id, &details.systems).await?;
                event(
                    &mut tx,
                    id,
                    now,
                    kinds::SPAWNED,
                    None,
                    Some(&incursion.state),
                )
                .await?;
                changes.push(pending(kinds::SPAWNED, id, None, Some(&incursion.state)));
                if incursion.has_boss {
                    changes.push(pending(kinds::BOSS_SPAWNED, id, None, None));
                }
                id
            }
            Some(row) => {
                update_spawn(&mut tx, row, incursion, details.as_ref(), now, &mut changes).await?;
                if (incursion.influence - row.influence).abs() > f64::EPSILON {
                    outcome.influence_changed.push(row.id);
                }
                row.id
            }
        };
        touched.insert(id);
        sqlx::query(
            "INSERT IGNORE INTO incursion_influence (spawn_id, sampled_at, influence) \
             VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(sampled_at)
        .bind(incursion.influence)
        .execute(&mut *tx)
        .await?;
    }

    let missing: Vec<&SpawnRow> = active.iter().filter(|s| !touched.contains(&s.id)).collect();
    if snapshot.incursions.is_empty() && !missing.is_empty() && !trust_empty {
        outcome.ignored_empty = true;
    } else {
        for row in missing {
            sqlx::query("UPDATE incursion_spawn SET ended_at = ? WHERE id = ?")
                .bind(now)
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            event(&mut tx, row.id, now, kinds::ENDED, Some(&row.state), None).await?;
            changes.push(pending(kinds::ENDED, row.id, Some(&row.state), None));
        }
    }

    sqlx::query(
        "INSERT INTO incursion_tracker_state (id, last_poll_at, data_modified_at) VALUES (1, ?, ?) \
         ON DUPLICATE KEY UPDATE last_poll_at = VALUES(last_poll_at), \
         data_modified_at = VALUES(data_modified_at)",
    )
    .bind(now)
    .bind(snapshot.last_modified.map(|t| t.timestamp()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Fill in the stored rows for the changes, and find the focus.
    let ids: Vec<i64> = changes.iter().map(|c| c.spawn_id).collect();
    let rows = store::spawns_by_id(db, &ids).await?;
    outcome.changes = changes
        .into_iter()
        .filter_map(|change| {
            Some(Change {
                kind: change.kind,
                from_state: change.from_state,
                to_state: change.to_state,
                spawn: rows.get(&change.spawn_id)?.clone(),
            })
        })
        .collect();
    let active = store::active_spawns(db).await?;
    outcome.focus = snapshot.incursions.iter().find_map(|incursion| {
        active
            .iter()
            .find(|s| s.constellation_id == incursion.constellation_id)
            .filter(|s| s.staging_security >= FOCUS_HIGHSEC)
            .map(|s| (s.constellation_id, s.constellation_name.clone()))
    });
    Ok(outcome)
}

/// A change recorded in the transaction; its spawn row is read after the commit.
struct Pending {
    kind: &'static str,
    spawn_id: i64,
    from_state: Option<String>,
    to_state: Option<String>,
}

fn pending(kind: &'static str, spawn_id: i64, from: Option<&str>, to: Option<&str>) -> Pending {
    Pending {
        kind,
        spawn_id,
        from_state: from.map(str::to_string),
        to_state: to.map(str::to_string),
    }
}

async fn stored_systems_differ(
    db: &Db,
    spawn_id: i64,
    incursion: &Incursion,
) -> Result<bool, sqlx::Error> {
    let stored: HashSet<i64> =
        sqlx::query_scalar("SELECT system_id FROM incursion_spawn_system WHERE spawn_id = ?")
            .bind(spawn_id)
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    let current: HashSet<i64> = infested(incursion).into_iter().collect();
    Ok(stored != current)
}

type Tx<'c> = sqlx::Transaction<'c, sqlx::MySql>;

async fn insert_spawn(
    tx: &mut Tx<'_>,
    incursion: &Incursion,
    details: &Resolved,
    now: i64,
    observed_from_start: bool,
) -> Result<i64, sqlx::Error> {
    let place = details.place.clone().unwrap_or(Place {
        constellation_name: incursion.constellation_id.to_string(),
        region_id: None,
        region_name: None,
    });
    let state = incursion.state.as_str();
    let result = sqlx::query(
        "INSERT INTO incursion_spawn (constellation_id, constellation_name, region_id, \
         region_name, faction_id, incursion_type, staging_system_id, staging_system_name, \
         staging_security, security_band, state, influence, has_boss, first_seen_at, \
         observed_from_start, last_seen_at, state_changed_at, mobilizing_at, withdrawing_at, \
         boss_spawned_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(incursion.constellation_id)
    .bind(&place.constellation_name)
    .bind(place.region_id)
    .bind(&place.region_name)
    .bind(incursion.faction_id)
    .bind(&incursion.type_)
    .bind(incursion.staging_solar_system_id)
    .bind(&details.staging.name)
    .bind(details.staging.security)
    .bind(SecurityBand::of(details.staging.security).as_str())
    .bind(state)
    .bind(incursion.influence)
    .bind(incursion.has_boss)
    .bind(now)
    .bind(observed_from_start)
    .bind(now)
    .bind(now)
    .bind((state == "mobilizing").then_some(now))
    .bind((state == "withdrawing").then_some(now))
    .bind(incursion.has_boss.then_some(now))
    .execute(&mut **tx)
    .await?;
    i64::try_from(result.last_insert_id()).map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

async fn replace_systems(
    tx: &mut Tx<'_>,
    spawn_id: i64,
    systems: &[SystemEntry],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM incursion_spawn_system WHERE spawn_id = ?")
        .bind(spawn_id)
        .execute(&mut **tx)
        .await?;
    if systems.is_empty() {
        return Ok(());
    }
    let mut query = sqlx::QueryBuilder::<sqlx::MySql>::new(
        "INSERT INTO incursion_spawn_system (spawn_id, system_id, system_name, security, role, \
         role_source) ",
    );
    query.push_values(systems, |mut row, (id, name, security, role, source)| {
        row.push_bind(spawn_id)
            .push_bind(*id)
            .push_bind(name.clone())
            .push_bind(*security)
            .push_bind(role.as_str())
            .push_bind(source.as_str());
    });
    query.build().execute(&mut **tx).await?;
    Ok(())
}

async fn event(
    tx: &mut Tx<'_>,
    spawn_id: i64,
    at: i64,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO incursion_event (spawn_id, at, kind, from_state, to_state) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(spawn_id)
    .bind(at)
    .bind(kind)
    .bind(from)
    .bind(to)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn update_spawn(
    tx: &mut Tx<'_>,
    row: &SpawnRow,
    incursion: &Incursion,
    details: Option<&Resolved>,
    now: i64,
    changes: &mut Vec<Pending>,
) -> Result<(), sqlx::Error> {
    let id = row.id;
    let mut state_changed_at = row.state_changed_at;
    let mut mobilizing_at = row.mobilizing_at;
    let mut withdrawing_at = row.withdrawing_at;
    let mut boss_spawned_at = row.boss_spawned_at;

    if row.state != incursion.state {
        state_changed_at = now;
        match incursion.state.as_str() {
            "mobilizing" => mobilizing_at = Some(now),
            "withdrawing" => withdrawing_at = Some(now),
            _ => {}
        }
        event(
            tx,
            id,
            now,
            kinds::STATE_CHANGED,
            Some(&row.state),
            Some(&incursion.state),
        )
        .await?;
        changes.push(pending(
            kinds::STATE_CHANGED,
            id,
            Some(&row.state),
            Some(&incursion.state),
        ));
    }
    if incursion.has_boss != row.has_boss() {
        let kind = if incursion.has_boss {
            boss_spawned_at = boss_spawned_at.or(Some(now));
            kinds::BOSS_SPAWNED
        } else {
            kinds::BOSS_GONE
        };
        event(tx, id, now, kind, None, None).await?;
        changes.push(pending(kind, id, None, None));
    }

    let (staging_name, staging_security) = match details {
        Some(details) if row.staging_system_id != incursion.staging_solar_system_id => {
            event(tx, id, now, kinds::STAGING_CHANGED, None, None).await?;
            changes.push(pending(kinds::STAGING_CHANGED, id, None, None));
            (details.staging.name.clone(), details.staging.security)
        }
        _ => (row.staging_system_name.clone(), row.staging_security),
    };
    if let Some(details) = details {
        replace_systems(tx, id, &details.systems).await?;
    }

    sqlx::query(
        "UPDATE incursion_spawn SET state = ?, influence = ?, has_boss = ?, last_seen_at = ?, \
         state_changed_at = ?, mobilizing_at = ?, withdrawing_at = ?, boss_spawned_at = ?, \
         staging_system_id = ?, staging_system_name = ?, staging_security = ?, \
         security_band = ? WHERE id = ?",
    )
    .bind(&incursion.state)
    .bind(incursion.influence)
    .bind(incursion.has_boss)
    .bind(now)
    .bind(state_changed_at)
    .bind(mobilizing_at)
    .bind(withdrawing_at)
    .bind(boss_spawned_at)
    .bind(incursion.staging_solar_system_id)
    .bind(staging_name)
    .bind(staging_security)
    .bind(SecurityBand::of(staging_security).as_str())
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
