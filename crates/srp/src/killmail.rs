//! Killmails behind reports: the raw ESI killmail and the name-enriched view.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use wl_esi::killmail::{KillmailData, parse_killmail_link};

use crate::Srp;
use crate::error::{SrpError, SrpResult};

/// A killmail with names resolved: names that could not be resolved are `null` (`Unknown` for
/// the victim's ship and items).
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct EnrichedKillmail {
    pub killmail_id: i64,
    /// As ESI formats it, e.g. `2024-05-01T12:00:00Z`.
    pub killmail_time: String,
    pub moon_id: Option<i64>,
    pub solar_system_id: i64,
    pub solar_system_name: Option<String>,
    pub war_id: Option<i64>,
    pub victim: EnrichedVictim,
    pub attackers: Vec<EnrichedAttacker>,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct EnrichedVictim {
    pub alliance_id: Option<i64>,
    pub alliance_name: Option<String>,
    pub character_id: Option<i64>,
    pub character_name: Option<String>,
    pub corporation_id: Option<i64>,
    pub corporation_name: Option<String>,
    pub damage_taken: i64,
    /// The faction of an NPC or faction-warfare victim; usually `null`. An addition to the
    /// legacy shape.
    pub faction_id: Option<i64>,
    pub faction_name: Option<String>,
    pub items: Vec<EnrichedItem>,
    pub ship_type_id: i64,
    pub ship_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct EnrichedAttacker {
    pub alliance_id: Option<i64>,
    pub alliance_name: Option<String>,
    pub character_id: Option<i64>,
    pub character_name: Option<String>,
    pub corporation_id: Option<i64>,
    pub corporation_name: Option<String>,
    pub damage_done: i64,
    /// The NPC faction of an NPC attacker (e.g. `500019`, Sansha's Nation), whose character,
    /// corporation and alliance are `null`; `null` for players. An addition to the legacy shape.
    pub faction_id: Option<i64>,
    pub faction_name: Option<String>,
    pub final_blow: bool,
    pub security_status: f64,
    pub ship_type_id: Option<i64>,
    pub ship_name: Option<String>,
    pub weapon_type_id: Option<i64>,
    pub weapon_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct EnrichedItem {
    pub flag: i32,
    pub item_type_id: i64,
    /// `Unknown` when the name could not be resolved.
    pub item_name: String,
    pub quantity_destroyed: Option<i64>,
    pub quantity_dropped: Option<i64>,
    pub singleton: i32,
}

/// Split a killmail link; malformed links are `BadRequest` with the parser's message.
pub fn parse_link(link: &str) -> SrpResult<(i64, String)> {
    parse_killmail_link(link).map_err(|e| SrpError::BadRequest(e.to_string()))
}

impl Srp {
    /// Fetch the killmail a link points to. Failures are `BadRequest`, as in the legacy waitlist.
    pub async fn killmail(&self, link: &str) -> SrpResult<KillmailData> {
        let (id, hash) = parse_link(link)?;
        self.esi()
            .killmail(id, &hash)
            .await
            .map_err(|e| SrpError::BadRequest(format!("Failed to fetch killmail: {e}")))
    }

    /// The killmail with every id resolved to a name (one bulk ESI lookup, 10 s timeout; names
    /// that cannot be resolved are `null`, or `Unknown` for the victim ship and items). Type
    /// names come from the static data when ESI doesn't answer.
    pub async fn enriched_killmail(&self, link: &str) -> SrpResult<EnrichedKillmail> {
        let km = self.killmail(link).await?;

        let mut ids = Vec::new();
        let v = &km.victim;
        ids.extend(
            [
                v.character_id,
                v.corporation_id,
                v.alliance_id,
                v.faction_id,
            ]
            .into_iter()
            .flatten(),
        );
        ids.push(v.ship_type_id);
        for a in &km.attackers {
            ids.extend(
                [
                    a.character_id,
                    a.corporation_id,
                    a.alliance_id,
                    a.faction_id,
                    a.ship_type_id,
                    a.weapon_type_id,
                ]
                .into_iter()
                .flatten(),
            );
        }
        ids.extend(v.items.iter().map(|i| i.item_type_id));
        ids.push(km.solar_system_id);
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(*id));

        let names: HashMap<i64, String> =
            tokio::time::timeout(Duration::from_secs(10), self.esi().names(&ids))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
        let name = |id: Option<i64>| id.and_then(|id| names.get(&id).cloned());
        let type_name = |id: Option<i64>| {
            name(id).or_else(|| {
                let id = i32::try_from(id?).ok()?;
                self.eve()?.name_of(id).map(str::to_string)
            })
        };

        Ok(EnrichedKillmail {
            killmail_id: km.killmail_id,
            killmail_time: km.killmail_time.clone(),
            moon_id: km.moon_id,
            solar_system_id: km.solar_system_id,
            solar_system_name: name(Some(km.solar_system_id)),
            war_id: km.war_id,
            victim: EnrichedVictim {
                alliance_id: v.alliance_id,
                alliance_name: name(v.alliance_id),
                character_id: v.character_id,
                character_name: name(v.character_id),
                corporation_id: v.corporation_id,
                corporation_name: name(v.corporation_id),
                damage_taken: v.damage_taken,
                faction_id: v.faction_id,
                faction_name: name(v.faction_id),
                items: v
                    .items
                    .iter()
                    .map(|i| EnrichedItem {
                        flag: i.flag,
                        item_type_id: i.item_type_id,
                        item_name: type_name(Some(i.item_type_id))
                            .unwrap_or_else(|| "Unknown".into()),
                        quantity_destroyed: i.quantity_destroyed,
                        quantity_dropped: i.quantity_dropped,
                        singleton: i.singleton,
                    })
                    .collect(),
                ship_type_id: v.ship_type_id,
                ship_name: type_name(Some(v.ship_type_id)).unwrap_or_else(|| "Unknown".into()),
            },
            attackers: km
                .attackers
                .iter()
                .map(|a| EnrichedAttacker {
                    alliance_id: a.alliance_id,
                    alliance_name: name(a.alliance_id),
                    character_id: a.character_id,
                    character_name: name(a.character_id),
                    corporation_id: a.corporation_id,
                    corporation_name: name(a.corporation_id),
                    damage_done: a.damage_done,
                    faction_id: a.faction_id,
                    faction_name: name(a.faction_id),
                    final_blow: a.final_blow,
                    security_status: a.security_status,
                    ship_type_id: a.ship_type_id,
                    ship_name: type_name(a.ship_type_id),
                    weapon_type_id: a.weapon_type_id,
                    weapon_name: type_name(a.weapon_type_id),
                })
                .collect(),
        })
    }

    /// Whether `character_id` was in a tracked fleet at `timestamp` (`fleet_activity`).
    pub async fn was_in_fleet(&self, character_id: i64, timestamp: i64) -> SrpResult<bool> {
        let row: Option<i64> = sqlx::query_scalar(
            "SELECT character_id FROM fleet_activity WHERE character_id = ? AND first_seen <= ? \
             AND last_seen >= ? LIMIT 1",
        )
        .bind(character_id)
        .bind(timestamp)
        .bind(timestamp)
        .fetch_optional(self.db())
        .await?;
        Ok(row.is_some())
    }
}
