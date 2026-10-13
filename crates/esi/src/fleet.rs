//! Fleet endpoints. Reads need `esi-fleets.read_fleet.v1`, changes `esi-fleets.write_fleet.v1`,
//! both on the character making the call (the fleet boss, except for the current-fleet lookup).

use serde::{Deserialize, Serialize};
use wl_core::types::TypeId;

use eve_esi_client::types::{
    PostFleetsFleetIdMembersBody, PostFleetsFleetIdMembersBodyRole, PutFleetsFleetIdBody,
};

use crate::error::{ErrorStyle, EsiResult};
use crate::esi::{Esi, send};
use crate::scope::Scope;
use crate::util::{enum_str, esi_time, type_id};

/// The fleet a character is in (`GET /characters/{id}/fleet`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterFleet {
    pub fleet_id: i64,
    pub fleet_boss_id: i64,
    /// `fleet_commander`, `wing_commander`, `squad_commander` or `squad_member`.
    pub role: String,
    pub squad_id: i64,
    pub wing_id: i64,
}

/// A fleet's settings (`GET /fleets/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetInfo {
    pub is_free_move: bool,
    pub is_registered: bool,
    pub motd: String,
}

/// A fleet member (`GET /fleets/{id}/members`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetMember {
    pub character_id: i64,
    pub ship_type_id: TypeId,
    pub squad_id: i64,
    pub wing_id: i64,
    /// `fleet_commander`, `wing_commander`, `squad_commander` or `squad_member`.
    pub role: String,
    pub role_name: String,
    pub solar_system_id: i64,
    pub station_id: Option<i64>,
    pub takes_fleet_warp: bool,
    /// When the member joined, as ESI formats it.
    pub join_time: String,
}

/// A fleet wing and its squads (`GET /fleets/{id}/wings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FleetWing {
    pub id: i64,
    pub name: String,
    pub squads: Vec<FleetSquad>,
}

/// A squad of a fleet wing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FleetSquad {
    pub id: i64,
    pub name: String,
}

impl Esi {
    /// The fleet `character_id` is in. ESI answers 404 when the character isn't in a fleet
    /// (check with [`crate::EsiErrorExt::is_not_found`]).
    pub async fn current_fleet(&self, character_id: i64) -> EsiResult<CharacterFleet> {
        let client = self
            .client_for(character_id, Scope::FleetsReadFleet)
            .await?;
        let fleet = send(
            client
                .get_characters_character_id_fleet()
                .character_id(character_id)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        Ok(CharacterFleet {
            fleet_id: fleet.fleet_id,
            fleet_boss_id: fleet.fleet_boss_id,
            role: enum_str(&fleet.role),
            squad_id: fleet.squad_id,
            wing_id: fleet.wing_id,
        })
    }

    /// `fleet_id`'s wings and squads, as its boss `boss_id` sees them. ESI answers 404 when
    /// `boss_id` isn't the fleet boss.
    pub async fn fleet_wings(&self, boss_id: i64, fleet_id: i64) -> EsiResult<Vec<FleetWing>> {
        let client = self.client_for(boss_id, Scope::FleetsReadFleet).await?;
        let wings = send(
            client.get_fleets_fleet_id_wings().fleet_id(fleet_id).send(),
            ErrorStyle::Body,
        )
        .await?;
        Ok(wings
            .0
            .into_iter()
            .map(|wing| FleetWing {
                id: wing.id,
                name: wing.name,
                squads: wing
                    .squads
                    .into_iter()
                    .map(|squad| FleetSquad {
                        id: squad.id,
                        name: squad.name,
                    })
                    .collect(),
            })
            .collect())
    }

    /// `fleet_id`'s members, read with the boss's token.
    pub async fn fleet_members(&self, boss_id: i64, fleet_id: i64) -> EsiResult<Vec<FleetMember>> {
        let client = self.client_for(boss_id, Scope::FleetsReadFleet).await?;
        let members = send(
            client
                .get_fleets_fleet_id_members()
                .fleet_id(fleet_id)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        members
            .0
            .into_iter()
            .map(|m| {
                Ok(FleetMember {
                    character_id: m.character_id,
                    ship_type_id: type_id(m.ship_type_id)?,
                    squad_id: m.squad_id,
                    wing_id: m.wing_id,
                    role: enum_str(&m.role),
                    role_name: m.role_name,
                    solar_system_id: m.solar_system_id,
                    station_id: m.station_id,
                    takes_fleet_warp: m.takes_fleet_warp,
                    join_time: esi_time(&m.join_time),
                })
            })
            .collect()
    }

    /// `fleet_id`'s settings, including the MOTD, read with the boss's token.
    pub async fn fleet_info(&self, boss_id: i64, fleet_id: i64) -> EsiResult<FleetInfo> {
        let client = self.client_for(boss_id, Scope::FleetsReadFleet).await?;
        let fleet = send(
            client.get_fleets_fleet_id().fleet_id(fleet_id).send(),
            ErrorStyle::Body,
        )
        .await?;
        Ok(FleetInfo {
            is_free_move: fleet.is_free_move,
            is_registered: fleet.is_registered,
            motd: fleet.motd,
        })
    }

    /// Replace `fleet_id`'s MOTD as its boss.
    pub async fn update_fleet_motd(
        &self,
        boss_id: i64,
        fleet_id: i64,
        motd: String,
    ) -> EsiResult<()> {
        let client = self.client_for(boss_id, Scope::FleetsWriteFleet).await?;
        let body = PutFleetsFleetIdBody {
            is_free_move: None,
            motd: Some(motd),
        };
        send(
            client
                .put_fleets_fleet_id()
                .fleet_id(fleet_id)
                .body(body)
                .send(),
            ErrorStyle::Message,
        )
        .await
    }

    /// Invite `character_id` into a squad of `fleet_id` as a squad member, acting as
    /// `fc_id`. ESI's refusal (e.g. the pilot is already in a fleet) comes back as
    /// `WithMessage(status, reason)`.
    pub async fn invite_to_fleet(
        &self,
        fc_id: i64,
        fleet_id: i64,
        character_id: i64,
        wing_id: i64,
        squad_id: i64,
    ) -> EsiResult<()> {
        let client = self.client_for(fc_id, Scope::FleetsWriteFleet).await?;
        let body = PostFleetsFleetIdMembersBody {
            character_id,
            role: PostFleetsFleetIdMembersBodyRole::SquadMember,
            squad_id: Some(squad_id),
            wing_id: Some(wing_id),
        };
        send(
            client
                .post_fleets_fleet_id_members()
                .fleet_id(fleet_id)
                .body(body)
                .send(),
            ErrorStyle::Message,
        )
        .await
    }

    /// Remove `member_id` from `fleet_id`, acting as `fc_id`. A member who already left gives
    /// `Status(404)`.
    pub async fn kick_fleet_member(
        &self,
        fc_id: i64,
        fleet_id: i64,
        member_id: i64,
    ) -> EsiResult<()> {
        let client = self.client_for(fc_id, Scope::FleetsWriteFleet).await?;
        send(
            client
                .delete_fleets_fleet_id_members_member_id()
                .fleet_id(fleet_id)
                .member_id(member_id)
                .send(),
            ErrorStyle::Status,
        )
        .await
    }
}
