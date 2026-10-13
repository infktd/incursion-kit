//! The tracker's tables (migration `0002_incursion_tracker.sql`): rows and queries.

use std::collections::HashMap;

use sqlx::{MySql, QueryBuilder};
use wl_db::Db;

/// Columns of [`SpawnRow`], in order.
macro_rules! spawn_columns {
    () => {
        "id, constellation_id, constellation_name, region_id, region_name, faction_id, \
         incursion_type, staging_system_id, staging_system_name, staging_security, \
         security_band, state, influence, has_boss, first_seen_at, observed_from_start, \
         last_seen_at, state_changed_at, mobilizing_at, withdrawing_at, boss_spawned_at, ended_at"
    };
}

/// One `incursion_spawn` row.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SpawnRow {
    pub id: i64,
    pub constellation_id: i64,
    pub constellation_name: String,
    pub region_id: Option<i64>,
    pub region_name: Option<String>,
    pub faction_id: i64,
    pub incursion_type: String,
    pub staging_system_id: i64,
    pub staging_system_name: String,
    pub staging_security: f64,
    pub security_band: String,
    pub state: String,
    pub influence: f64,
    pub has_boss: i8,
    pub first_seen_at: i64,
    pub observed_from_start: i8,
    pub last_seen_at: i64,
    pub state_changed_at: i64,
    pub mobilizing_at: Option<i64>,
    pub withdrawing_at: Option<i64>,
    pub boss_spawned_at: Option<i64>,
    pub ended_at: Option<i64>,
}

impl SpawnRow {
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }

    pub fn has_boss(&self) -> bool {
        self.has_boss != 0
    }

    pub fn observed_from_start(&self) -> bool {
        self.observed_from_start != 0
    }
}

/// One `incursion_spawn_system` row.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SystemRow {
    pub spawn_id: i64,
    pub system_id: i64,
    pub system_name: String,
    pub security: f64,
    pub role: String,
    pub role_source: String,
}

/// One `incursion_event` row.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EventRow {
    pub id: i64,
    pub spawn_id: i64,
    pub at: i64,
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

/// `incursion_tracker_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct TrackerState {
    pub last_poll_at: i64,
    pub data_modified_at: Option<i64>,
}

/// Event kinds.
pub mod kinds {
    pub const SPAWNED: &str = "spawned";
    pub const STATE_CHANGED: &str = "state_changed";
    pub const BOSS_SPAWNED: &str = "boss_spawned";
    pub const BOSS_GONE: &str = "boss_gone";
    pub const STAGING_CHANGED: &str = "staging_changed";
    pub const ENDED: &str = "ended";
}

/// Active spawns, oldest first.
pub async fn active_spawns<'e>(
    db: impl sqlx::Executor<'e, Database = MySql>,
) -> Result<Vec<SpawnRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        spawn_columns!(),
        " FROM incursion_spawn WHERE ended_at IS NULL ORDER BY id"
    ))
    .fetch_all(db)
    .await
}

pub async fn spawn(db: &Db, id: i64) -> Result<Option<SpawnRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        spawn_columns!(),
        " FROM incursion_spawn WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Filters for [`ended_spawns`].
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub band: Option<String>,
    pub constellation_id: Option<i64>,
    pub region_id: Option<i64>,
}

fn push_history_filter(query: &mut QueryBuilder<MySql>, filter: &HistoryFilter) {
    query.push(" WHERE ended_at IS NOT NULL");
    if let Some(band) = &filter.band {
        query.push(" AND security_band = ").push_bind(band.clone());
    }
    if let Some(id) = filter.constellation_id {
        query.push(" AND constellation_id = ").push_bind(id);
    }
    if let Some(id) = filter.region_id {
        query.push(" AND region_id = ").push_bind(id);
    }
}

/// Ended spawns, most recently ended first, and how many match in total.
pub async fn ended_spawns(
    db: &Db,
    filter: &HistoryFilter,
    offset: u64,
    limit: u64,
) -> Result<(Vec<SpawnRow>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<MySql>::new("SELECT COUNT(*) FROM incursion_spawn");
    push_history_filter(&mut count, filter);
    let total: i64 = count.build_query_scalar().fetch_one(db).await?;

    let mut rows = QueryBuilder::<MySql>::new(concat!(
        "SELECT ",
        spawn_columns!(),
        " FROM incursion_spawn"
    ));
    push_history_filter(&mut rows, filter);
    rows.push(" ORDER BY ended_at DESC, id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows = rows.build_query_as().fetch_all(db).await?;
    Ok((rows, total))
}

/// The `limit` most recently ended spawns of a band.
pub async fn recently_ended(db: &Db, band: &str, limit: u32) -> Result<Vec<SpawnRow>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        spawn_columns!(),
        " FROM incursion_spawn WHERE security_band = ? AND ended_at IS NOT NULL \
         ORDER BY ended_at DESC, id DESC LIMIT ?"
    ))
    .bind(band)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The systems of the given spawns.
pub async fn systems(
    db: &Db,
    spawn_ids: &[i64],
) -> Result<HashMap<i64, Vec<SystemRow>>, sqlx::Error> {
    let mut out: HashMap<i64, Vec<SystemRow>> = HashMap::new();
    if spawn_ids.is_empty() {
        return Ok(out);
    }
    let mut query = QueryBuilder::<MySql>::new(
        "SELECT spawn_id, system_id, system_name, security, role, role_source \
         FROM incursion_spawn_system WHERE spawn_id IN (",
    );
    let mut ids = query.separated(", ");
    for id in spawn_ids {
        ids.push_bind(*id);
    }
    query.push(") ORDER BY spawn_id, system_id");
    for row in query.build_query_as::<SystemRow>().fetch_all(db).await? {
        out.entry(row.spawn_id).or_default().push(row);
    }
    Ok(out)
}

/// Influence samples `(sampled_at, influence)` of the given spawns since `since`, oldest first.
pub async fn influence(
    db: &Db,
    spawn_ids: &[i64],
    since: Option<i64>,
) -> Result<HashMap<i64, Vec<(i64, f64)>>, sqlx::Error> {
    let mut out: HashMap<i64, Vec<(i64, f64)>> = HashMap::new();
    if spawn_ids.is_empty() {
        return Ok(out);
    }
    let mut query = QueryBuilder::<MySql>::new(
        "SELECT spawn_id, sampled_at, influence FROM incursion_influence WHERE spawn_id IN (",
    );
    let mut ids = query.separated(", ");
    for id in spawn_ids {
        ids.push_bind(*id);
    }
    query.push(")");
    if let Some(since) = since {
        query.push(" AND sampled_at >= ").push_bind(since);
    }
    query.push(" ORDER BY spawn_id, sampled_at");
    for (spawn_id, at, value) in query
        .build_query_as::<(i64, i64, f64)>()
        .fetch_all(db)
        .await?
    {
        out.entry(spawn_id).or_default().push((at, value));
    }
    Ok(out)
}

/// The events of one spawn, oldest first.
pub async fn events(db: &Db, spawn_id: i64) -> Result<Vec<EventRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, spawn_id, at, kind, from_state, to_state FROM incursion_event \
         WHERE spawn_id = ? ORDER BY at, id",
    )
    .bind(spawn_id)
    .fetch_all(db)
    .await
}

/// Events of every spawn, newest first, older than event `before_id` when given.
pub async fn recent_events(
    db: &Db,
    before_id: Option<i64>,
    limit: u64,
) -> Result<Vec<EventRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, spawn_id, at, kind, from_state, to_state FROM incursion_event \
         WHERE (? IS NULL OR id < ?) ORDER BY id DESC LIMIT ?",
    )
    .bind(before_id)
    .bind(before_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Spawns by id (any state).
pub async fn spawns_by_id(db: &Db, ids: &[i64]) -> Result<HashMap<i64, SpawnRow>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut query = QueryBuilder::<MySql>::new(concat!(
        "SELECT ",
        spawn_columns!(),
        " FROM incursion_spawn WHERE id IN ("
    ));
    let mut list = query.separated(", ");
    for id in ids {
        list.push_bind(*id);
    }
    query.push(")");
    Ok(query
        .build_query_as::<SpawnRow>()
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect())
}

pub async fn tracker_state<'e>(
    db: impl sqlx::Executor<'e, Database = MySql>,
) -> Result<Option<TrackerState>, sqlx::Error> {
    sqlx::query_as(
        "SELECT last_poll_at, data_modified_at FROM incursion_tracker_state WHERE id = 1",
    )
    .fetch_optional(db)
    .await
}
