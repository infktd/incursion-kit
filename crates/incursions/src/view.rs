//! The public API's JSON shapes (`docs/incursions-api.md`), built from the stored spawns and
//! enriched from the star map: per-system security, size, stations and jumps, jumps from the
//! configured reference systems, and the maximum remaining time.

use std::collections::HashMap;

use serde::Serialize;
use wl_db::Db;
use wl_eve_data::universe::{Jumps, SolarSystem, display_security};
use wl_eve_data::{SecurityBand, Universe};

use crate::roles::Role;
use crate::rules::{self, WindowStatus, rfc3339};
use crate::store::{self, EventRow, HistoryFilter, SpawnRow, SystemRow};

/// How far back the influence history of the active list goes.
pub const LIST_HISTORY_SECONDS: i64 = 72 * 3600;

/// What enrichment needs: the star map (empty without map data), the reference systems and the
/// current time.
pub struct Context<'a> {
    universe: &'a Universe,
    references: Vec<(&'a SolarSystem, Jumps, Jumps)>,
    now: i64,
}

impl<'a> Context<'a> {
    /// `reference_names`: systems to count jumps from (unknown names are skipped).
    pub fn new(universe: &'a Universe, reference_names: &[String], now: i64) -> Self {
        let references = reference_names
            .iter()
            .filter_map(|name| universe.system_by_name(name))
            .map(|system| {
                let shortest = universe.jumps_from(system.id, |_| true);
                let highsec = universe.jumps_from(system.id, |s| {
                    SecurityBand::of(s.security) == SecurityBand::Highsec
                });
                (system, shortest, highsec)
            })
            .collect();
        Self {
            universe,
            references,
            now,
        }
    }

    pub fn now(&self) -> i64 {
        self.now
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct Named {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct RegionRef {
    pub id: Option<i64>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct FactionRef {
    pub id: i64,
    pub name: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct SystemRef {
    pub id: i64,
    pub name: String,
    /// As shown in game (one decimal).
    pub security: f64,
    /// Unrounded.
    pub security_status: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct Lifetime {
    /// When the current state began (the first sighting if the tracker missed the change).
    pub state_since: String,
    /// False when the tracker first saw the spawn already in this state (it went live, or was
    /// down): the maximum is then an upper bound measured from that sighting.
    pub state_since_observed: bool,
    pub max_despawn_at: Option<String>,
    pub max_remaining_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct StationView {
    pub id: i64,
    pub name: String,
    pub has_repair: bool,
    pub has_cloning: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct SystemView {
    pub id: i64,
    pub name: String,
    pub security: f64,
    pub security_status: f64,
    pub role: Role,
    /// `known` (ESI or a published layout), `estimated` (stargate graph) or `unknown`.
    pub role_source: String,
    pub jumps_from_staging: Option<u32>,
    /// Sun to the farthest planet, stargate or station.
    pub radius_au: Option<f64>,
    /// Longest distance between two of them: roughly the longest warp.
    pub longest_warp_au: Option<f64>,
    pub stations: Vec<StationView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct ReferenceJumps {
    pub system_id: i64,
    pub system_name: String,
    /// Shortest route to the staging system.
    pub jumps: Option<u32>,
    /// Shortest route through high-sec only (`null` if there is none).
    pub highsec_jumps: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct EventView {
    pub id: i64,
    pub spawn_id: i64,
    pub at: String,
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

/// A spawn. Lists leave out the parts marked optional.
#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct SpawnView {
    pub id: i64,
    pub active: bool,
    pub constellation: Named,
    pub region: RegionRef,
    pub faction: FactionRef,
    #[serde(rename = "type")]
    pub incursion_type: String,
    pub security_band: String,
    pub staging_system: SystemRef,
    pub state: String,
    pub influence: f64,
    pub has_boss: bool,
    pub first_seen_at: String,
    /// True when the tracker was running when the spawn appeared, so `first_seen_at` is its
    /// spawn time (within one ESI refresh).
    pub observed_from_start: bool,
    pub last_seen_at: String,
    pub state_changed_at: String,
    pub mobilizing_at: Option<String>,
    pub withdrawing_at: Option<String>,
    pub boss_spawned_at: Option<String>,
    pub ended_at: Option<String>,
    /// From the first sighting to the end (or now).
    pub duration_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifetime: Option<Lifetime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub systems: Option<Vec<SystemView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_jumps: Option<Vec<ReferenceJumps>>,
    /// `[unix_seconds, influence]` pairs, oldest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Vec<Vec<f64>>>)]
    pub influence_history: Option<Vec<(i64, f64)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<EventView>>,
}

fn faction_name(id: i64) -> Option<&'static str> {
    match id {
        500_019 => Some("Sansha's Nation"),
        _ => None,
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn summary(row: &SpawnRow, now: i64) -> SpawnView {
    SpawnView {
        id: row.id,
        active: row.is_active(),
        constellation: Named {
            id: row.constellation_id,
            name: row.constellation_name.clone(),
        },
        region: RegionRef {
            id: row.region_id,
            name: row.region_name.clone(),
        },
        faction: FactionRef {
            id: row.faction_id,
            name: faction_name(row.faction_id),
        },
        incursion_type: row.incursion_type.clone(),
        security_band: row.security_band.clone(),
        staging_system: SystemRef {
            id: row.staging_system_id,
            name: row.staging_system_name.clone(),
            security: display_security(row.staging_security),
            security_status: row.staging_security,
        },
        state: if row.is_active() {
            row.state.clone()
        } else {
            "ended".into()
        },
        influence: row.influence,
        has_boss: row.has_boss(),
        first_seen_at: rfc3339(row.first_seen_at),
        observed_from_start: row.observed_from_start(),
        last_seen_at: rfc3339(row.last_seen_at),
        state_changed_at: rfc3339(row.state_changed_at),
        mobilizing_at: row.mobilizing_at.map(rfc3339),
        withdrawing_at: row.withdrawing_at.map(rfc3339),
        boss_spawned_at: row.boss_spawned_at.map(rfc3339),
        ended_at: row.ended_at.map(rfc3339),
        duration_seconds: (row.ended_at.unwrap_or(now) - row.first_seen_at).max(0),
        lifetime: None,
        systems: None,
        reference_jumps: None,
        influence_history: None,
        events: None,
    }
}

fn lifetime(row: &SpawnRow, now: i64) -> Lifetime {
    let max = rules::max_despawn_at(&row.state, row.state_changed_at);
    Lifetime {
        state_since: rfc3339(row.state_changed_at),
        state_since_observed: row.observed_from_start() || row.state_changed_at > row.first_seen_at,
        max_despawn_at: max.map(rfc3339),
        max_remaining_seconds: max.map(|at| (at - now).max(0)),
    }
}

fn systems(row: &SpawnRow, stored: &[SystemRow], ctx: &Context<'_>) -> Vec<SystemView> {
    let from_staging = ctx.universe.jumps_from(row.staging_system_id, |_| true);
    let mut out: Vec<SystemView> = stored
        .iter()
        .map(|s| {
            let map = ctx.universe.system(s.system_id);
            SystemView {
                id: s.system_id,
                name: s.system_name.clone(),
                security: display_security(s.security),
                security_status: s.security,
                role: Role::parse(&s.role),
                role_source: s.role_source.clone(),
                jumps_from_staging: from_staging.to(s.system_id),
                radius_au: map.map(|m| round1(m.radius_au)),
                longest_warp_au: map.map(|m| round1(m.longest_warp_au)),
                stations: ctx
                    .universe
                    .stations(s.system_id)
                    .iter()
                    .map(|st| StationView {
                        id: st.id,
                        name: st.name.clone(),
                        has_repair: st.has_repair,
                        has_cloning: st.has_cloning,
                    })
                    .collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.role.cmp(&b.role).then_with(|| a.name.cmp(&b.name)));
    out
}

fn reference_jumps(row: &SpawnRow, ctx: &Context<'_>) -> Vec<ReferenceJumps> {
    ctx.references
        .iter()
        .map(|(system, shortest, highsec)| ReferenceJumps {
            system_id: system.id,
            system_name: system.name.clone(),
            jumps: shortest.to(row.staging_system_id),
            highsec_jumps: highsec.to(row.staging_system_id),
        })
        .collect()
}

fn event_view(row: EventRow) -> EventView {
    EventView {
        id: row.id,
        spawn_id: row.spawn_id,
        at: rfc3339(row.at),
        kind: row.kind,
        from_state: row.from_state,
        to_state: row.to_state,
    }
}

/// The latest sample of every hour.
fn hourly(samples: &[(i64, f64)]) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = Vec::new();
    for &(at, value) in samples {
        match out.last_mut() {
            Some(last) if last.0.div_euclid(3600) == at.div_euclid(3600) => *last = (at, value),
            _ => out.push((at, value)),
        }
    }
    out
}

/// Every active spawn, fully enriched, with an hourly influence history of the last 72 hours.
/// High-sec first, then by staging security (highest first), like eve-incursions.de.
pub async fn active(db: &Db, ctx: &Context<'_>) -> Result<Vec<SpawnView>, sqlx::Error> {
    let rows = store::active_spawns(db).await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut stored = store::systems(db, &ids).await?;
    let mut influence = store::influence(db, &ids, Some(ctx.now - LIST_HISTORY_SECONDS)).await?;
    let mut out: Vec<SpawnView> = rows
        .iter()
        .map(|row| {
            let mut view = summary(row, ctx.now);
            view.lifetime = Some(lifetime(row, ctx.now));
            view.systems = Some(systems(
                row,
                &stored.remove(&row.id).unwrap_or_default(),
                ctx,
            ));
            view.reference_jumps = Some(reference_jumps(row, ctx));
            view.influence_history = Some(hourly(&influence.remove(&row.id).unwrap_or_default()));
            view
        })
        .collect();
    out.sort_by(|a, b| {
        b.staging_system
            .security_status
            .total_cmp(&a.staging_system.security_status)
    });
    Ok(out)
}

/// One spawn (active or ended) with its full influence history and events.
pub async fn detail(db: &Db, ctx: &Context<'_>, id: i64) -> Result<Option<SpawnView>, sqlx::Error> {
    let Some(row) = store::spawn(db, id).await? else {
        return Ok(None);
    };
    let stored = store::systems(db, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default();
    let influence = store::influence(db, &[id], None)
        .await?
        .remove(&id)
        .unwrap_or_default();
    let events = store::events(db, id).await?;
    let mut view = summary(&row, ctx.now);
    if row.is_active() {
        view.lifetime = Some(lifetime(&row, ctx.now));
    }
    view.systems = Some(systems(&row, &stored, ctx));
    view.reference_jumps = Some(reference_jumps(&row, ctx));
    view.influence_history = Some(influence);
    view.events = Some(events.into_iter().map(event_view).collect());
    Ok(Some(view))
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct HistoryPage {
    pub items: Vec<SpawnView>,
    pub page: u64,
    pub per_page: u64,
    pub total: i64,
}

/// Ended spawns, most recent first (summaries without systems or history).
pub async fn history(
    db: &Db,
    now: i64,
    filter: &HistoryFilter,
    page: u64,
    per_page: u64,
) -> Result<HistoryPage, sqlx::Error> {
    let page = page.max(1);
    let (rows, total) = store::ended_spawns(db, filter, (page - 1) * per_page, per_page).await?;
    Ok(HistoryPage {
        items: rows.iter().map(|row| summary(row, now)).collect(),
        page,
        per_page,
        total,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct LogItem {
    #[serde(flatten)]
    pub event: EventView,
    pub constellation: Named,
    pub security_band: String,
    pub staging_system: Named,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct LogPage {
    pub items: Vec<LogItem>,
    /// Pass as `before` for the next (older) page; `null` at the end.
    pub next_before: Option<i64>,
}

/// State changes of every spawn, newest first.
pub async fn log(db: &Db, before: Option<i64>, limit: u64) -> Result<LogPage, sqlx::Error> {
    let events = store::recent_events(db, before, limit).await?;
    let ids: Vec<i64> = events.iter().map(|e| e.spawn_id).collect();
    let spawns = store::spawns_by_id(db, &ids).await?;
    let next_before = (events.len() as u64 == limit)
        .then(|| events.last().map(|e| e.id))
        .flatten();
    let items = events
        .into_iter()
        .filter_map(|event| {
            let spawn = spawns.get(&event.spawn_id)?;
            Some(LogItem {
                constellation: Named {
                    id: spawn.constellation_id,
                    name: spawn.constellation_name.clone(),
                },
                security_band: spawn.security_band.clone(),
                staging_system: Named {
                    id: spawn.staging_system_id,
                    name: spawn.staging_system_name.clone(),
                },
                event: event_view(event),
            })
        })
        .collect();
    Ok(LogPage { items, next_before })
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct WindowView {
    /// The ended spawn whose slot this window refills.
    pub after_spawn_id: i64,
    pub after_constellation: Named,
    pub ended_at: String,
    pub opens_at: String,
    pub closes_at: String,
    pub status: WindowStatus,
    pub seconds_until_open: i64,
    pub seconds_until_close: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct BandWindows {
    pub band: String,
    pub slots: u32,
    pub active: u32,
    pub free_slots: u32,
    pub windows: Vec<WindowView>,
    /// Free slots without a recorded end (the tracker wasn't running when it happened).
    pub unknown_windows: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, utoipa::ToSchema)]
pub struct WindowsView {
    pub generated_at: String,
    pub opens_after_hours: i64,
    pub closes_after_hours: i64,
    pub bands: Vec<BandWindows>,
}

/// Spawn windows per band. `slots` maps each band to its concurrent incursion count.
pub async fn windows(
    db: &Db,
    now: i64,
    slots: &HashMap<SecurityBand, u32>,
) -> Result<WindowsView, sqlx::Error> {
    let active = store::active_spawns(db).await?;
    let mut bands = Vec::new();
    for band in SecurityBand::ALL {
        let slots = slots.get(&band).copied().unwrap_or(0);
        let active = active
            .iter()
            .filter(|s| s.security_band == band.as_str())
            .count() as u32;
        let free_slots = slots.saturating_sub(active);
        let ended = if free_slots > 0 {
            store::recently_ended(db, band.as_str(), free_slots).await?
        } else {
            Vec::new()
        };
        let windows: Vec<WindowView> = ended
            .iter()
            .filter_map(|row| {
                let ended_at = row.ended_at?;
                let (opens, closes, status) = rules::window(ended_at, now);
                Some(WindowView {
                    after_spawn_id: row.id,
                    after_constellation: Named {
                        id: row.constellation_id,
                        name: row.constellation_name.clone(),
                    },
                    ended_at: rfc3339(ended_at),
                    opens_at: rfc3339(opens),
                    closes_at: rfc3339(closes),
                    status,
                    seconds_until_open: (opens - now).max(0),
                    seconds_until_close: (closes - now).max(0),
                })
            })
            .collect();
        bands.push(BandWindows {
            band: band.as_str().into(),
            slots,
            active,
            free_slots,
            unknown_windows: free_slots - windows.len() as u32,
            windows,
        });
    }
    Ok(WindowsView {
        generated_at: rfc3339(now),
        opens_after_hours: rules::WINDOW_OPENS_AFTER / 3600,
        closes_after_hours: rules::WINDOW_CLOSES_AFTER / 3600,
        bands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_keeps_the_last_sample_per_hour() {
        let samples = [(0, 0.1), (300, 0.2), (3600, 0.3), (3900, 0.4), (7300, 0.5)];
        assert_eq!(hourly(&samples), [(300, 0.2), (3900, 0.4), (7300, 0.5)]);
        assert!(hourly(&[]).is_empty());
    }
}
