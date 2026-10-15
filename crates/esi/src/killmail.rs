//! Killmails (public, addressed by ID and hash).

use serde::{Deserialize, Serialize};

use crate::error::{ErrorStyle, EsiError, EsiResult};
use crate::esi::{Esi, send};
use crate::universe::Position;
use crate::util::esi_time;

/// A killmail, shaped as the legacy waitlist returned it from its SRP endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KillmailData {
    pub attackers: Vec<KillmailAttacker>,
    pub killmail_id: i64,
    /// As ESI formats it, e.g. `2024-05-01T12:00:00Z`.
    pub killmail_time: String,
    pub moon_id: Option<i64>,
    pub solar_system_id: i64,
    pub victim: KillmailVictim,
    pub war_id: Option<i64>,
}

/// An attacker on a killmail (character, corporation and alliance ids are absent for NPCs, which
/// carry their `faction_id` instead).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KillmailAttacker {
    pub alliance_id: Option<i64>,
    pub character_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub damage_done: i64,
    /// The NPC faction of an NPC attacker (e.g. `500019`, Sansha's Nation); `null` for players.
    /// An addition to the legacy shape.
    #[serde(default)]
    pub faction_id: Option<i64>,
    pub final_blow: bool,
    pub security_status: f64,
    /// Type id of the attacker's ship.
    pub ship_type_id: Option<i64>,
    /// Type id of the weapon (or drone) that did the damage.
    pub weapon_type_id: Option<i64>,
}

/// The killmail's victim (`character_id` is absent when the victim is not a character).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KillmailVictim {
    pub alliance_id: Option<i64>,
    pub character_id: Option<i64>,
    pub corporation_id: Option<i64>,
    pub damage_taken: i64,
    /// The faction of an NPC or faction-warfare victim; usually `null`. An addition to the
    /// legacy shape.
    #[serde(default)]
    pub faction_id: Option<i64>,
    pub items: Vec<KillmailItem>,
    /// Missing for some killmails (ESI marks it optional).
    pub position: Option<Position>,
    /// Type id of the lost ship.
    pub ship_type_id: i64,
}

/// A fitted or carried item. Container contents (ESI's nested `items`) are not kept, as in the
/// legacy waitlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct KillmailItem {
    /// The inventory flag (slot or hold) the item was in.
    pub flag: i32,
    pub item_type_id: i64,
    pub quantity_destroyed: Option<i64>,
    pub quantity_dropped: Option<i64>,
    /// ESI's `singleton` value.
    pub singleton: i32,
}

impl Esi {
    pub async fn killmail(&self, killmail_id: i64, hash: &str) -> EsiResult<KillmailData> {
        let km = send(
            self.client()
                .get_killmails_killmail_id_killmail_hash()
                .killmail_id(killmail_id)
                .killmail_hash(hash)
                .send(),
            ErrorStyle::Status,
        )
        .await?;
        Ok(KillmailData {
            attackers: km
                .attackers
                .into_iter()
                .map(|a| KillmailAttacker {
                    alliance_id: a.alliance_id,
                    character_id: a.character_id,
                    corporation_id: a.corporation_id,
                    damage_done: a.damage_done,
                    faction_id: a.faction_id,
                    final_blow: a.final_blow,
                    security_status: a.security_status,
                    ship_type_id: a.ship_type_id,
                    weapon_type_id: a.weapon_type_id,
                })
                .collect(),
            killmail_id: km.killmail_id,
            killmail_time: esi_time(&km.killmail_time),
            moon_id: km.moon_id,
            solar_system_id: km.solar_system_id,
            victim: KillmailVictim {
                alliance_id: km.victim.alliance_id,
                character_id: km.victim.character_id,
                corporation_id: km.victim.corporation_id,
                damage_taken: km.victim.damage_taken,
                faction_id: km.victim.faction_id,
                items: km
                    .victim
                    .items
                    .into_iter()
                    .map(|i| KillmailItem {
                        flag: i32::try_from(i.flag).unwrap_or_default(),
                        item_type_id: i.item_type_id,
                        quantity_destroyed: i.quantity_destroyed,
                        quantity_dropped: i.quantity_dropped,
                        singleton: i32::try_from(i.singleton).unwrap_or_default(),
                    })
                    .collect(),
                position: km.victim.position.map(|p| Position {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                }),
                ship_type_id: km.victim.ship_type_id,
            },
            war_id: km.war_id,
        })
    }
}

/// Split an ESI killmail link (`https://esi.evetech.net/latest/killmails/<id>/<hash>/`, with or
/// without the trailing slash) into its ID and hash. Malformed links are
/// `WithMessage(400, ..)`, as in the legacy waitlist.
pub fn parse_killmail_link(link: &str) -> EsiResult<(i64, String)> {
    let invalid = |msg: &str| EsiError::WithMessage(400, msg.to_string());
    let parts: Vec<&str> = link.split('/').collect();
    let at = parts
        .iter()
        .position(|&p| p == "killmails")
        .ok_or_else(|| invalid("Invalid killmail URL format"))?;
    if at + 2 >= parts.len() {
        return Err(invalid("Invalid killmail URL format"));
    }
    let id = parts[at + 1]
        .parse::<i64>()
        .map_err(|_| invalid("Invalid killmail ID"))?;
    let hash = parts[at + 2];
    if hash.is_empty() {
        return Err(invalid("Invalid killmail URL format"));
    }
    Ok((id, hash.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_killmail_links_like_the_legacy_waitlist() {
        let expected = (128982873, "abcdef".to_string());
        for link in [
            "https://esi.evetech.net/latest/killmails/128982873/abcdef/",
            "https://esi.evetech.net/latest/killmails/128982873/abcdef",
            "https://esi.evetech.net/killmails/128982873/abcdef/?datasource=tranquility",
        ] {
            let (id, hash) = parse_killmail_link(link).unwrap();
            assert_eq!(id, expected.0, "{link}");
            assert!(hash.starts_with(&expected.1), "{link}");
        }
        for bad in [
            "https://zkillboard.com/kill/1/",
            "https://esi.evetech.net/latest/killmails/abc/hash/",
            "https://esi.evetech.net/latest/killmails/1/",
            "https://esi.evetech.net/latest/killmails/1",
        ] {
            assert!(
                matches!(parse_killmail_link(bad), Err(EsiError::WithMessage(400, _))),
                "{bad}"
            );
        }
    }
}
