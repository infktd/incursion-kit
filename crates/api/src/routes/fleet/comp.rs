//! `GET /api/fleet/members` and `GET /api/fleet/fleetcomp`.

use std::collections::{BTreeMap, HashMap};

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use wl_core::types::{Hull, TypeId};
use wl_esi::fleet::FleetMember as EsiFleetMember;
use wl_fitting::FleetRules;

use super::{
    CharacterQuery, MembersByName, authorize_view, character_names, current_fleet_id, fleet_wings,
    hull, motd, registered_boss,
};
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

#[derive(Serialize, ToSchema)]
pub(super) struct FleetMembersResponse {
    members: Vec<FleetMembersMember>,
}

#[derive(Serialize, ToSchema)]
struct FleetMembersMember {
    /// Character ID.
    id: i64,
    /// Stored name, `null` for characters unknown to the waitlist.
    name: Option<String>,
    /// The ship flown (`Unknown type <id>` for a type missing from the static data).
    ship: Hull,
    /// Waitlist category name of the member's squad (from the registration).
    wl_category: Option<String>,
    /// ESI squad name.
    category: Option<String>,
    /// ESI fleet role: `fleet_commander`, `wing_commander`, `squad_commander` or
    /// `squad_member`.
    role: String,
}

/// Fleet members
///
/// The members of the character's registered fleet, with their ship and the waitlist category
/// of their squad. Requires `fleet-view`, and `character_id` must be one of the caller's own
/// characters (main or alt). ESI is read as `character_id` (its fleet and wings; it must be the
/// fleet boss) and as the registered boss (the members), with `esi-fleets.read_fleet.v1`.
/// ESI failures are passed on with ESI's status as `ESI error: {"error":"…"}`.
#[utoipa::path(
    get,
    path = "/api/fleet/members",
    tag = tags::FLEET,
    security(("session" = ["fleet-view"])),
    params(CharacterQuery),
    responses(
        (status = 200, description = "The fleet's members.", body = FleetMembersResponse),
        (status = 404, description = "`You are not in a fleet`, `Fleet not configured` (not registered) or `You are not the fleet boss`.", body = String, content_type = "text/plain"),
    ),
)]
pub(super) async fn fleet_members(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<FleetMembersResponse>> {
    authorize_view(&state, &account, query.character_id).await?;
    let fleet_id = current_fleet_id(&state, query.character_id).await?;
    let boss_id = registered_boss(&state, fleet_id).await?;

    let members = state.esi()?.fleet_members(boss_id, fleet_id).await?;
    let ids: Vec<i64> = members.iter().map(|m| m.character_id).collect();
    let names = character_names(&state, &ids).await?;

    let data = state.data_store()?.get();
    let squads: HashMap<i64, String> =
        sqlx::query_as("SELECT squad_id, category FROM fleet_squad WHERE fleet_id = ?")
            .bind(fleet_id)
            .fetch_all(state.db())
            .await?
            .into_iter()
            .collect();
    let wings = fleet_wings(&state, query.character_id, fleet_id).await?;
    let squad_names: HashMap<i64, &str> = wings
        .iter()
        .flat_map(|wing| &wing.squads)
        .map(|squad| (squad.id, squad.name.as_str()))
        .collect();
    let eve = data.eve();

    Ok(Json(FleetMembersResponse {
        members: members
            .into_iter()
            .map(|member| FleetMembersMember {
                id: member.character_id,
                name: names.get(&member.character_id).cloned(),
                ship: hull(eve, member.ship_type_id),
                wl_category: squads
                    .get(&member.squad_id)
                    .and_then(|category| data.categories.name_of(category))
                    .map(str::to_string),
                category: squad_names.get(&member.squad_id).map(|s| s.to_string()),
                role: member.role,
            })
            .collect(),
    }))
}

#[derive(Serialize, ToSchema)]
#[schema(as = FleetCompositionResponse)]
pub(super) struct FleetCompResponse {
    wings: Vec<FleetCompWing>,
    /// ESI fleet ID.
    id: i64,
    /// The fleet commander.
    member: Option<FleetCompMember>,
    /// MOTD role name (as configured) → the pilots listed under it. `null` when the MOTD is
    /// empty, assigns nobody, or couldn't be read.
    role_assignments: Option<BTreeMap<String, Vec<RoleAssignment>>>,
}

#[derive(Serialize, ToSchema)]
struct FleetCompWing {
    id: i64,
    name: String,
    squads: Vec<FleetCompSquad>,
    /// The wing commander.
    member: Option<FleetCompMember>,
}

#[derive(Serialize, ToSchema)]
struct FleetCompSquad {
    id: i64,
    name: String,
    members: Vec<FleetCompMember>,
}

#[derive(Serialize, ToSchema)]
struct FleetCompMember {
    /// Character ID.
    id: i64,
    /// Stored name, `null` for characters unknown to the waitlist.
    name: Option<String>,
    /// The ship flown (`Unknown type <id>` for a type missing from the static data).
    ship: Hull,
    /// ESI fleet role: `fleet_commander`, `wing_commander`, `squad_commander` or
    /// `squad_member`.
    role: String,
    /// The character's corporation or alliance is at war.
    at_war: bool,
}

#[derive(Serialize, Debug, PartialEq, Eq, ToSchema)]
struct RoleAssignment {
    /// As written in the MOTD.
    name: String,
    in_fleet: bool,
    /// Whether the ship flown is one of the role's hulls; `null` when not in fleet.
    correct_hull: Option<bool>,
    /// The ship type flown, when in fleet.
    hull_id: Option<TypeId>,
}

/// Fleet composition
///
/// The character's registered fleet by wing and squad (with the fleet and wing commanders),
/// each member's ship and war status, and the role assignments read from the fleet MOTD.
/// Requires `fleet-view`, and `character_id` must be one of the caller's own characters (main
/// or alt). ESI is read as `character_id` (its fleet and wings; it must be the fleet boss) and
/// as the registered boss (members and MOTD), with `esi-fleets.read_fleet.v1`. ESI failures are
/// passed on with ESI's status as `ESI error: {"error":"…"}`, except that a MOTD that can't be
/// read only makes `role_assignments` `null`.
#[utoipa::path(
    get,
    path = "/api/fleet/fleetcomp",
    tag = tags::FLEET,
    security(("session" = ["fleet-view"])),
    params(CharacterQuery),
    responses(
        (status = 200, description = "The fleet by wing and squad.", body = FleetCompResponse),
        (status = 404, description = "`You are not in a fleet`, `Fleet not configured` (not registered) or `You are not the fleet boss`.", body = String, content_type = "text/plain"),
    ),
)]
pub(super) async fn fleet_composition(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<FleetCompResponse>> {
    authorize_view(&state, &account, query.character_id).await?;
    let esi = state.esi()?;
    let fleet_id = current_fleet_id(&state, query.character_id).await?;
    let boss_id = registered_boss(&state, fleet_id).await?;
    let wings = fleet_wings(&state, query.character_id, fleet_id).await?;
    let members = esi.fleet_members(boss_id, fleet_id).await?;
    let ids: Vec<i64> = members.iter().map(|m| m.character_id).collect();
    let names = character_names(&state, &ids).await?;
    let at_war = state
        .war_cache()
        .character_ids_at_war(state.db(), &ids)
        .await?;
    let data = state.data_store()?.get();
    let eve = data.eve();

    let member = |m: &EsiFleetMember| FleetCompMember {
        id: m.character_id,
        name: names.get(&m.character_id).cloned(),
        ship: hull(eve, m.ship_type_id),
        role: m.role.clone(),
        at_war: at_war.contains(&m.character_id),
    };
    let commander = members
        .iter()
        .find(|m| m.role == "fleet_commander")
        .map(member);
    let wings = wings
        .into_iter()
        .map(|wing| FleetCompWing {
            id: wing.id,
            member: members
                .iter()
                .find(|m| m.wing_id == wing.id && m.role == "wing_commander")
                .map(member),
            name: wing.name,
            squads: wing
                .squads
                .into_iter()
                .map(|squad| FleetCompSquad {
                    id: squad.id,
                    members: members
                        .iter()
                        .filter(|m| m.squad_id == squad.id)
                        .map(member)
                        .collect(),
                    name: squad.name,
                })
                .collect(),
        })
        .collect();

    // A MOTD that can't be read only drops the role assignments.
    let role_assignments = match esi.fleet_info(boss_id, fleet_id).await {
        Ok(info) => role_assignments(
            &info.motd,
            &data.rules.fleet,
            &MembersByName::new(&members, &names),
        ),
        Err(e) => {
            tracing::debug!(fleet_id, error = %e, "could not read the fleet MOTD");
            None
        }
    };

    Ok(Json(FleetCompResponse {
        wings,
        id: fleet_id,
        member: commander,
        role_assignments,
    }))
}

/// The MOTD's role assignments with each pilot's fleet and hull status.
fn role_assignments(
    text: &str,
    rules: &FleetRules,
    members: &MembersByName,
) -> Option<BTreeMap<String, Vec<RoleAssignment>>> {
    let parsed = motd::parse_roles(text, &rules.role_names());
    if parsed.is_empty() {
        return None;
    }
    Some(
        parsed
            .into_iter()
            .map(|(role, names)| {
                let hulls = rules.role(&role).map(|r| &r.hulls);
                let assignments = names
                    .into_iter()
                    .map(|name| {
                        let ship = members.ships.get(&name.to_lowercase()).copied();
                        RoleAssignment {
                            in_fleet: ship.is_some(),
                            correct_hull: ship
                                .map(|ship| hulls.is_some_and(|hulls| hulls.contains(&ship))),
                            hull_id: ship,
                            name,
                        }
                    })
                    .collect();
                (role, assignments)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use wl_fitting::MotdRole;

    #[test]
    fn assignments_report_fleet_and_hull_status() {
        let rules = FleetRules {
            motd_roles: vec![
                MotdRole {
                    name: "DDD".into(),
                    hulls: BTreeSet::from([17740]),
                },
                MotdRole {
                    name: "LR".into(),
                    hulls: BTreeSet::from([28661]),
                },
            ],
            motd_link_type: 1377,
        };
        let members = MembersByName {
            ids: HashMap::from([("one".into(), 1), ("two".into(), 2)]),
            ships: HashMap::from([("one".into(), 17740), ("two".into(), 17740)]),
        };
        let motd = "DDD: <a href=\"showinfo:1377//1\">One</a><br>LR: <a href=\"x\">Two</a> \
                    <a href=\"x\">Gone</a>";
        let out = role_assignments(motd, &rules, &members).unwrap();
        assert_eq!(
            out["DDD"],
            [RoleAssignment {
                name: "One".into(),
                in_fleet: true,
                correct_hull: Some(true),
                hull_id: Some(17740),
            }]
        );
        assert_eq!(
            out["LR"],
            [
                RoleAssignment {
                    name: "Two".into(),
                    in_fleet: true,
                    correct_hull: Some(false),
                    hull_id: Some(17740),
                },
                RoleAssignment {
                    name: "Gone".into(),
                    in_fleet: false,
                    correct_hull: None,
                    hull_id: None,
                },
            ]
        );
        assert!(role_assignments("", &rules, &members).is_none());
        assert!(role_assignments("no roles", &rules, &members).is_none());
    }
}
