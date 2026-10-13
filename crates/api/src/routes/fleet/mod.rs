//! Fleet registration, status, members, composition and MOTD role assignment (ESI-backed).
//!
//! - `GET /api/fleet/status` (`fleet-view`): every registered fleet and its boss.
//! - `GET /api/fleet/info?character_id=` (`fleet-view`, own character): the character's current
//!   fleet and its wings/squads (the character must be the boss).
//! - `POST /api/fleet/register` (`fleet-configure`, own character): register a fleet with a squad
//!   per waitlist category. `200 OK`.
//! - `POST /api/fleet/close` (`fleet-configure`, own character): kick everyone else from the
//!   character's fleet. Plain-text summary.
//! - `GET /api/fleet/members?character_id=` (`fleet-view`, own character): members of the
//!   character's registered fleet with their waitlist category.
//! - `GET /api/fleet/fleetcomp?character_id=` (`fleet-view`, own character): the fleet by wing
//!   and squad, with at-war flags and the MOTD role assignments.
//! - `POST /api/fleet/update-motd` (`fleet-configure`, own character): assign a pilot to a MOTD
//!   role (roles and their hulls come from `rules.yaml`, `fleet.motd_roles`).
//!
//! "Own character" means the main or one of its alts. A character not in a fleet gets
//! `404 You are not in a fleet`; one that isn't the boss gets `404 You are not the fleet boss`
//! where wings are read; an unregistered fleet gets `404 Fleet not configured`.

mod comp;
mod motd;

use std::collections::{HashMap, HashSet};

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Character, Hull, TypeId};
use wl_esi::EsiErrorExt as _;
use wl_esi::fleet::{FleetMember, FleetWing};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(fleet_status))
        .routes(routes!(fleet_info))
        .routes(routes!(close_fleet))
        .routes(routes!(comp::fleet_members))
        .routes(routes!(register_fleet))
        .routes(routes!(comp::fleet_composition))
        .routes(routes!(update_motd))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The caller's own character (main or alt) whose fleet to read.
    character_id: i64,
}

/// Check `fleet-view` and that the account may act for `character_id`.
async fn authorize_view(
    state: &AppState,
    account: &AuthenticatedAccount,
    character_id: i64,
) -> ApiResult<()> {
    account.require_access("fleet-view")?;
    account
        .authorize_character(state.db(), character_id, None)
        .await
}

/// The fleet `character_id` is in.
async fn current_fleet_id(state: &AppState, character_id: i64) -> ApiResult<i64> {
    match state.esi()?.current_fleet(character_id).await {
        Ok(fleet) => Ok(fleet.fleet_id),
        Err(e) if e.is_not_found() => Err(ApiError::not_found("You are not in a fleet")),
        Err(e) => Err(e.into()),
    }
}

/// The fleet's wings as `character_id` (who must be its boss) sees them.
async fn fleet_wings(
    state: &AppState,
    character_id: i64,
    fleet_id: i64,
) -> ApiResult<Vec<FleetWing>> {
    match state.esi()?.fleet_wings(character_id, fleet_id).await {
        Ok(wings) => Ok(wings),
        Err(e) if e.is_not_found() => Err(ApiError::not_found("You are not the fleet boss")),
        Err(e) => Err(e.into()),
    }
}

/// The registered boss of `fleet_id`.
async fn registered_boss(state: &AppState, fleet_id: i64) -> ApiResult<i64> {
    sqlx::query_scalar("SELECT boss_id FROM fleet WHERE id = ?")
        .bind(fleet_id)
        .fetch_optional(state.db())
        .await?
        .ok_or_else(|| ApiError::not_found("Fleet not configured"))
}

/// Stored names of these characters (unknown ones are absent).
async fn character_names(state: &AppState, ids: &[i64]) -> ApiResult<HashMap<i64, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut query = QueryBuilder::new("SELECT id, name FROM `character` WHERE id IN (");
    let mut list = query.separated(", ");
    for id in ids {
        list.push_bind(*id);
    }
    query.push(")");
    let rows: Vec<(i64, String)> = query.build_query_as().fetch_all(state.db()).await?;
    Ok(rows.into_iter().collect())
}

/// `{id, name}` of a ship. A type missing from the static data (a ship newer than it) is named
/// after its ID rather than failing the whole fleet view.
fn hull(eve: &wl_eve_data::EveData, id: TypeId) -> Hull {
    let name = match eve.name_of(id) {
        Some(name) => name.to_string(),
        None => {
            tracing::warn!(type_id = id, "ship type missing from the static data");
            format!("Unknown type {id}")
        }
    };
    Hull { id, name }
}

/// Fleet members by lowercased name: their character ID and ship (members unknown to the
/// database are left out, as in the legacy waitlist).
struct MembersByName {
    ids: HashMap<String, i64>,
    ships: HashMap<String, TypeId>,
}

impl MembersByName {
    fn new(members: &[FleetMember], names: &HashMap<i64, String>) -> Self {
        let mut ids = HashMap::new();
        let mut ships = HashMap::new();
        for member in members {
            if let Some(name) = names.get(&member.character_id) {
                let key = name.to_lowercase();
                ids.insert(key.clone(), member.character_id);
                ships.insert(key, member.ship_type_id);
            }
        }
        Self { ids, ships }
    }

    fn names(&self) -> HashSet<String> {
        self.ids.keys().cloned().collect()
    }
}

// --- status / info ---------------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
struct FleetStatusFleet {
    /// ESI fleet ID.
    id: i64,
    /// The character that registered the fleet (`corporation_id` is always `null`).
    boss: Character,
}

#[derive(Serialize, ToSchema)]
struct FleetStatusResponse {
    fleets: Vec<FleetStatusFleet>,
}

/// Registered fleets
///
/// Every fleet registered with `POST /api/fleet/register`, with the boss that registered it.
/// Requires `fleet-view`. Reads only the database (no ESI call), so a fleet that has since
/// closed stays listed until the fleet updater drops it.
#[utoipa::path(
    get,
    path = "/api/fleet/status",
    tag = tags::FLEET,
    security(("session" = ["fleet-view"])),
    responses((status = 200, description = "The registered fleets.", body = FleetStatusResponse)),
)]
async fn fleet_status(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
) -> ApiResult<Json<FleetStatusResponse>> {
    account.require_access("fleet-view")?;
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT fleet.id, fleet.boss_id, `character`.name FROM fleet \
         JOIN `character` ON fleet.boss_id = `character`.id",
    )
    .fetch_all(state.db())
    .await?;
    Ok(Json(FleetStatusResponse {
        fleets: rows
            .into_iter()
            .map(|(id, boss_id, name)| FleetStatusFleet {
                id,
                boss: Character {
                    id: boss_id,
                    name,
                    corporation_id: None,
                },
            })
            .collect(),
    }))
}

#[derive(Serialize, ToSchema)]
struct FleetInfoResponse {
    /// ESI fleet ID.
    fleet_id: i64,
    /// The fleet's wings and squads, as ESI lists them.
    wings: Vec<FleetWing>,
}

/// Current fleet layout
///
/// The fleet the character is in and its wings and squads (used to pick the squads before
/// registering it). Requires `fleet-view`, and `character_id` must be one of the caller's own
/// characters (main or alt). Reads ESI as that character (`esi-fleets.read_fleet.v1`; without
/// the scope or token the answer is 401 `ESI error: missing ESI scope` / `ESI error: no ESI
/// token found`); the character must be the fleet boss. Other ESI failures are passed on with
/// ESI's status as `ESI error: {"error":"…"}`.
#[utoipa::path(
    get,
    path = "/api/fleet/info",
    tag = tags::FLEET,
    security(("session" = ["fleet-view"])),
    params(CharacterQuery),
    responses(
        (status = 200, description = "The fleet and its wings.", body = FleetInfoResponse),
        (status = 404, description = "`You are not in a fleet`, or `You are not the fleet boss`.", body = String, content_type = "text/plain"),
    ),
)]
async fn fleet_info(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<FleetInfoResponse>> {
    authorize_view(&state, &account, query.character_id).await?;
    let fleet_id = current_fleet_id(&state, query.character_id).await?;
    let wings = fleet_wings(&state, query.character_id, fleet_id).await?;
    Ok(Json(FleetInfoResponse { fleet_id, wings }))
}

// --- register / close ------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
struct RegisterRequest {
    /// The fleet boss: one of the caller's own characters.
    character_id: i64,
    /// ESI fleet ID.
    fleet_id: i64,
    /// Waitlist category id (from `categories.yaml`) → `[wing_id, squad_id]`. Every
    /// configured category needs an entry; extra keys are ignored.
    #[schema(value_type = HashMap<String, Vec<i64>>, example = json!({"logi": [1, 2], "dps": [1, 3]}))]
    assignments: HashMap<String, (i64, i64)>,
}

/// Register a fleet
///
/// Makes `character_id` the boss of `fleet_id` and stores which squad each waitlist category
/// is invited into, replacing any previous registration of the fleet. Requires
/// `fleet-configure`, and `character_id` must be one of the caller's own characters. No ESI
/// call is made: the fleet and squad IDs are not checked.
#[utoipa::path(
    post,
    path = "/api/fleet/register",
    tag = tags::FLEET,
    security(("session" = ["fleet-configure"])),
    request_body = RegisterRequest,
    responses(
        (status = 200, description = "Registered.", body = String, content_type = "text/plain", example = "OK"),
        (status = 400, description = "`Missing assignment for <category name>` when a configured category has no squad (nothing is saved).", body = String, content_type = "text/plain"),
    ),
)]
async fn register_fleet(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<RegisterRequest>,
) -> ApiResult<&'static str> {
    account.require_access("fleet-configure")?;
    account
        .authorize_character(state.db(), input.character_id, None)
        .await?;
    let data = state.data_store()?.get();

    let mut tx = state.db().begin().await?;
    sqlx::query("DELETE FROM fleet_squad WHERE fleet_id = ?")
        .bind(input.fleet_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("REPLACE INTO fleet (id, boss_id) VALUES (?, ?)")
        .bind(input.fleet_id)
        .bind(input.character_id)
        .execute(&mut *tx)
        .await?;
    for category in &data.categories.categories {
        let Some((wing_id, squad_id)) = input.assignments.get(&category.id) else {
            // Dropping the transaction rolls everything back.
            return Err(ApiError::bad_request(format!(
                "Missing assignment for {}",
                category.name
            )));
        };
        sqlx::query(
            "INSERT INTO fleet_squad (fleet_id, wing_id, squad_id, category) VALUES (?, ?, ?, ?)",
        )
        .bind(input.fleet_id)
        .bind(wing_id)
        .bind(squad_id)
        .bind(&category.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok("OK")
}

#[derive(Deserialize, ToSchema)]
struct CloseRequest {
    /// The fleet boss: one of the caller's own characters.
    character_id: i64,
}

/// Close the fleet
///
/// Kicks every other member out of the fleet `character_id` is in, via ESI as that character
/// (`esi-fleets.read_fleet.v1` and `esi-fleets.write_fleet.v1`; it must be the fleet boss).
/// Requires `fleet-configure`, and `character_id` must be one of the caller's own characters.
/// Members who already left are skipped. The fleet's registration is left as is. The answer is
/// a plain-text summary: `All fleet members kicked.` or `Removed <kicked> of <others> fleet
/// members.`. ESI failures while reading the members are passed on with ESI's status as `ESI
/// error: {"error":"…"}`; a failed kick is a 500.
#[utoipa::path(
    post,
    path = "/api/fleet/close",
    tag = tags::FLEET,
    security(("session" = ["fleet-configure"])),
    request_body = CloseRequest,
    responses(
        (status = 200, description = "A summary of the kicks.", body = String, content_type = "text/plain", example = "All fleet members kicked."),
        (status = 404, description = "`You are not in a fleet`.", body = String, content_type = "text/plain"),
    ),
)]
async fn close_fleet(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<CloseRequest>,
) -> ApiResult<String> {
    account
        .authorize_character(state.db(), input.character_id, None)
        .await?;
    account.require_access("fleet-configure")?;

    let esi = state.esi()?;
    let fleet_id = current_fleet_id(&state, input.character_id).await?;
    let members = esi.fleet_members(input.character_id, fleet_id).await?;
    let total = members.len();
    let mut kicked = 0;
    for member in members {
        if member.character_id == input.character_id {
            continue;
        }
        match esi
            .kick_fleet_member(input.character_id, fleet_id, member.character_id)
            .await
        {
            Ok(()) => kicked += 1,
            Err(e) if e.is_not_found() => {} // already gone
            Err(e) => return Err(e.into()),
        }
    }

    let others = total.saturating_sub(1);
    if kicked == others {
        Ok("All fleet members kicked.".to_string())
    } else {
        Ok(format!("Removed {kicked} of {others} fleet members."))
    }
}

// --- MOTD ------------------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
struct UpdateMotdRequest {
    /// One of the caller's own characters, in the fleet.
    character_id: i64,
    /// MOTD role name (case-insensitive; one of `fleet.motd_roles` in `rules.yaml`, though an
    /// unknown role is accepted and written in upper case).
    role: String,
    /// The pilot to assign, as named in game.
    character_name: String,
}

#[derive(Serialize, ToSchema)]
struct UpdateMotdResponse {
    /// Always `true`.
    success: bool,
    /// Always `MOTD updated successfully`.
    message: String,
    /// The pilot was already listed under the role.
    is_duplicate: bool,
    /// The pilot is in fleet in a ship that isn't one of the role's hulls.
    has_incorrect_hull: bool,
}

/// Assign a MOTD role
///
/// Puts a pilot under a role in the fleet MOTD (`ROLE: <pilot link>` lines), drops pilots who
/// left the fleet from every role, and saves the MOTD. Requires `fleet-configure`, and
/// `character_id` must be one of the caller's own characters; it must be in a registered
/// fleet. ESI is called as the registered boss (`esi-fleets.read_fleet.v1` and
/// `esi-fleets.write_fleet.v1`). The pilot is always added: the flags only tell the client
/// what to warn about. ESI failures are passed on with ESI's status as `ESI error: …`.
#[utoipa::path(
    post,
    path = "/api/fleet/update-motd",
    tag = tags::FLEET,
    security(("session" = ["fleet-configure"])),
    request_body = UpdateMotdRequest,
    responses(
        (status = 200, description = "The MOTD was saved.", body = UpdateMotdResponse),
        (status = 404, description = "`You are not in a fleet`, or `Fleet not configured` when the fleet isn't registered.", body = String, content_type = "text/plain"),
    ),
)]
async fn update_motd(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<UpdateMotdRequest>,
) -> ApiResult<Json<UpdateMotdResponse>> {
    account.require_access("fleet-configure")?;
    account
        .authorize_character(state.db(), input.character_id, None)
        .await?;
    let data = state.data_store()?.get();
    let rules = &data.rules.fleet;
    let roles = rules.role_names();
    let esi = state.esi()?;

    let fleet_id = current_fleet_id(&state, input.character_id).await?;
    let boss_id = registered_boss(&state, fleet_id).await?;
    let current = esi.fleet_info(boss_id, fleet_id).await?.motd;
    let assignments = motd::parse_roles(&current, &roles);

    let members = esi.fleet_members(boss_id, fleet_id).await?;
    let ids: Vec<i64> = members.iter().map(|m| m.character_id).collect();
    let names = character_names(&state, &ids).await?;
    let by_name = MembersByName::new(&members, &names);
    let in_fleet = by_name.names();

    let role = rules.role(&input.role);
    let role_name = role.map_or_else(|| input.role.to_uppercase(), |r| r.name.clone());
    let name_lower = input.character_name.to_lowercase();
    let is_duplicate = assignments
        .iter()
        .find(|(listed, _)| listed.eq_ignore_ascii_case(&role_name))
        .is_some_and(|(_, names)| names.iter().any(|n| n.to_lowercase() == name_lower));
    let has_incorrect_hull = by_name
        .ships
        .get(&name_lower)
        .is_some_and(|ship| !role.is_some_and(|r| r.hulls.contains(ship)));

    let character_id = by_name.ids.get(&name_lower).copied().unwrap_or(0);
    let updated = motd::add_to_role(
        &current,
        &role_name,
        &input.character_name,
        character_id,
        &roles,
        &in_fleet,
        rules.motd_link_type,
    );
    let updated = motd::remove_absent(&updated, &roles, &in_fleet);
    esi.update_fleet_motd(boss_id, fleet_id, updated).await?;

    Ok(Json(UpdateMotdResponse {
        success: true,
        message: "MOTD updated successfully".to_string(),
        is_duplicate,
        has_incorrect_hull,
    }))
}
