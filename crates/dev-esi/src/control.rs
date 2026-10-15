//! The control API (`/_dev/*`, JSON) and the dashboard (`/_dev`): change the world while the
//! waitlist runs, e.g. from a frontend's end-to-end tests. Errors are `{"error": "..."}`.
//!
//! | | |
//! |---|---|
//! | `GET /_dev/health` | `ok` |
//! | `GET /_dev/state` | everything: settings, characters, corporations, alliances, fleets, invites, killmails, wars, incursions |
//! | `POST /_dev/reset` | back to the seed (logins stay valid) |
//! | `GET`, `PUT /_dev/settings` | `{invite_accept_delay_ms, incursions_cache_secs, default_layout}` (PUT: any subset) |
//! | `GET`, `POST /_dev/characters` | list (with fleet and pending invite) / add a character (seed format) |
//! | `GET`, `PATCH /_dev/characters/{id}` | one character with its effective skills / change fields (shallow merge, seed format) |
//! | `PUT /_dev/characters/{id}/skills` | `{"skill_set": "elite" or null, "skills": {"3336": 5}}` |
//! | `PUT /_dev/characters/{id}/implants` | `[20499, ...]` |
//! | `POST /_dev/characters/{id}/revoke` | revoke its refresh tokens |
//! | `POST /_dev/characters/{id}/leave` | leave its fleet |
//! | `GET`, `POST /_dev/fleets` | list / form a fleet: `{boss_id, motd?}` |
//! | `GET`, `DELETE /_dev/fleets/{id}` | one fleet / disband it |
//! | `POST /_dev/fleets/{id}/members` | put a pilot in: `{character_id, role?, wing_id?, squad_id?, squad?}` (`squad`: a squad name) |
//! | `DELETE /_dev/fleets/{id}/members/{character_id}` | take a pilot out |
//! | `PUT /_dev/fleets/{id}/boss` | `{character_id}`: make a member the boss |
//! | `GET /_dev/invites`, `POST /_dev/invites/{character_id}/accept` or `/decline` | pending invites |
//! | `POST /_dev/killmails` | add a killmail (ESI format plus optional `killmail_hash`); answers its ID, hash and ESI link |
//! | `POST /_dev/corporations/{id}/journal` | add a wallet journal entry (ESI format plus `division`, default 1); moves the balance |
//! | `PUT /_dev/corporations/{id}/wallets` | `[{division, balance}]` |
//! | `GET`, `PUT /_dev/incursions`, `GET`, `PUT /_dev/wars` | replace the lists (ESI format) |
//! | `GET /_dev/events?after=<id>` | what happened: logins, tokens, invites, joins, kicks, MOTD changes, windows opened |

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::DevEsi;
use crate::esi::EsiJson;
use crate::seed::{Character, Incursion, JournalEntry, Killmail, Wallet, War, WingLayout};
use crate::world::{DevError, Role, World};

type ControlResult = Result<Response, DevError>;

pub fn routes() -> Router<DevEsi> {
    Router::new()
        .route("/_dev", get(dashboard))
        .route("/_dev/", get(dashboard))
        .route("/_dev/health", get(|| async { "ok" }))
        .route("/_dev/state", get(state))
        .route("/_dev/reset", post(reset))
        .route("/_dev/settings", get(settings).put(update_settings))
        .route("/_dev/characters", get(characters).post(add_character))
        .route(
            "/_dev/characters/{id}",
            get(character).patch(patch_character),
        )
        .route("/_dev/characters/{id}/skills", put(set_skills))
        .route("/_dev/characters/{id}/implants", put(set_implants))
        .route("/_dev/characters/{id}/revoke", post(revoke))
        .route("/_dev/characters/{id}/leave", post(leave))
        .route("/_dev/fleets", get(fleets).post(create_fleet))
        .route("/_dev/fleets/{id}", get(fleet).delete(disband))
        .route("/_dev/fleets/{id}/members", post(join))
        .route(
            "/_dev/fleets/{id}/members/{character_id}",
            axum::routing::delete(remove_member),
        )
        .route("/_dev/fleets/{id}/boss", put(set_boss))
        .route("/_dev/invites", get(invites))
        .route("/_dev/invites/{character_id}/accept", post(accept))
        .route("/_dev/invites/{character_id}/decline", post(decline))
        .route("/_dev/killmails", post(add_killmail))
        .route("/_dev/corporations/{id}/journal", post(add_journal))
        .route("/_dev/corporations/{id}/wallets", put(set_wallets))
        .route("/_dev/incursions", get(incursions).put(set_incursions))
        .route("/_dev/wars", get(wars).put(set_wars))
        .route("/_dev/events", get(events))
}

fn ok() -> ControlResult {
    Ok(Json(json!({"ok": true})).into_response())
}

/// A character with where it is in fleet.
fn character_view(world: &World, id: i64, with_skills: bool) -> Result<Value, DevError> {
    let character = world.character(id)?;
    let mut view = serde_json::to_value(character).unwrap_or_default();
    view["alliance_id"] = json!(world.alliance_of(character));
    view["fleet"] = match world.fleet_of(id) {
        Some((fleet, member)) => json!({
            "fleet_id": fleet.fleet_id,
            "is_boss": fleet.boss_id == id,
            "role": member.role,
            "wing_id": member.wing_id,
            "squad_id": member.squad_id,
            "role_name": World::role_name(fleet, member),
        }),
        None => Value::Null,
    };
    view["pending_invite"] = json!(world.invites.get(&id));
    if with_skills {
        view["effective_skills"] = json!(world.skills_of(character));
    }
    Ok(view)
}

async fn state(State(dev): State<DevEsi>) -> ControlResult {
    let world = dev.world();
    let characters: Vec<Value> = world
        .characters
        .keys()
        .map(|id| character_view(&world, *id, false))
        .collect::<Result<_, _>>()?;
    Ok(Json(json!({
        "settings": world.settings,
        "characters": characters,
        "corporations": world.corporations.values().collect::<Vec<_>>(),
        "alliances": world.alliances.values().collect::<Vec<_>>(),
        "fleets": world.fleets.values().collect::<Vec<_>>(),
        "invites": world.invites.values().collect::<Vec<_>>(),
        "killmails": world.killmails.values().collect::<Vec<_>>(),
        "wars": world.wars.values().collect::<Vec<_>>(),
        "incursions": world.incursions,
        "skill_sets": world.skill_sets.keys().collect::<Vec<_>>(),
    }))
    .into_response())
}

async fn reset(State(dev): State<DevEsi>) -> ControlResult {
    dev.world()
        .reset()
        .map_err(|e| DevError::new(500, e.to_string()))?;
    ok()
}

async fn settings(State(dev): State<DevEsi>) -> ControlResult {
    Ok(Json(json!(dev.world().settings)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsUpdate {
    invite_accept_delay_ms: Option<u64>,
    incursions_cache_secs: Option<u64>,
    default_layout: Option<Vec<WingLayout>>,
}

async fn update_settings(
    State(dev): State<DevEsi>,
    EsiJson(update): EsiJson<SettingsUpdate>,
) -> ControlResult {
    let mut world = dev.world();
    if let Some(delay) = update.invite_accept_delay_ms {
        world.settings.invite_accept_delay_ms = delay;
    }
    if let Some(secs) = update.incursions_cache_secs {
        world.settings.incursions_cache_secs = secs;
    }
    if let Some(layout) = update.default_layout {
        if layout.iter().all(|w| w.squads.is_empty()) {
            return Err(DevError::bad_request("the layout needs at least one squad"));
        }
        world.settings.default_layout = layout;
    }
    Ok(Json(json!(world.settings)).into_response())
}

async fn characters(State(dev): State<DevEsi>) -> ControlResult {
    let world = dev.world();
    let list: Vec<Value> = world
        .characters
        .keys()
        .map(|id| character_view(&world, *id, false))
        .collect::<Result<_, _>>()?;
    Ok(Json(list).into_response())
}

async fn character(State(dev): State<DevEsi>, Path(id): Path<i64>) -> ControlResult {
    Ok(Json(character_view(&dev.world(), id, true)?).into_response())
}

async fn add_character(
    State(dev): State<DevEsi>,
    EsiJson(character): EsiJson<Character>,
) -> ControlResult {
    let id = character.id;
    let mut world = dev.world();
    world.add_character(character)?;
    Ok((
        StatusCode::CREATED,
        Json(character_view(&world, id, false)?),
    )
        .into_response())
}

/// Shallow-merge `patch` into the character (seed format), keeping its ID.
fn patched(world: &World, id: i64, patch: Value) -> Result<Character, DevError> {
    let Value::Object(patch) = patch else {
        return Err(DevError::bad_request("expected a JSON object"));
    };
    let mut current = serde_json::to_value(world.character(id)?).unwrap_or_default();
    for (key, value) in patch {
        current[key] = value;
    }
    current["id"] = json!(id);
    serde_json::from_value(current).map_err(|e| DevError::bad_request(e.to_string()))
}

async fn patch_character(
    State(dev): State<DevEsi>,
    Path(id): Path<i64>,
    EsiJson(patch): EsiJson<Value>,
) -> ControlResult {
    let mut world = dev.world();
    let character = patched(&world, id, patch)?;
    world.update_character(character)?;
    Ok(Json(character_view(&world, id, true)?).into_response())
}

async fn set_skills(
    State(dev): State<DevEsi>,
    Path(id): Path<i64>,
    EsiJson(body): EsiJson<Value>,
) -> ControlResult {
    let mut patch = serde_json::Map::new();
    if let Some(set) = body.get("skill_set") {
        patch.insert("skill_set".into(), set.clone());
    }
    patch.insert(
        "skills".into(),
        body.get("skills").cloned().unwrap_or_else(|| json!({})),
    );
    patch_character(State(dev), Path(id), EsiJson(Value::Object(patch))).await
}

async fn set_implants(
    State(dev): State<DevEsi>,
    Path(id): Path<i64>,
    EsiJson(implants): EsiJson<Vec<i64>>,
) -> ControlResult {
    patch_character(
        State(dev),
        Path(id),
        EsiJson(json!({ "implants": implants })),
    )
    .await
}

async fn revoke(State(dev): State<DevEsi>, Path(id): Path<i64>) -> ControlResult {
    let mut world = dev.world();
    world.revoke_tokens(id)?;
    dev.save_grants(&world);
    ok()
}

async fn leave(State(dev): State<DevEsi>, Path(id): Path<i64>) -> ControlResult {
    let mut world = dev.world();
    world.character(id)?;
    world
        .leave_fleet(id)
        .ok_or_else(|| DevError::not_found("Character is not in a fleet"))?;
    ok()
}

async fn fleets(State(dev): State<DevEsi>) -> ControlResult {
    let world = dev.world();
    Ok(Json(world.fleets.values().collect::<Vec<_>>()).into_response())
}

async fn fleet(State(dev): State<DevEsi>, Path(id): Path<i64>) -> ControlResult {
    Ok(Json(json!(dev.world().fleet(id)?)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewFleet {
    boss_id: i64,
    #[serde(default)]
    motd: String,
}

async fn create_fleet(
    State(dev): State<DevEsi>,
    EsiJson(body): EsiJson<NewFleet>,
) -> ControlResult {
    let mut world = dev.world();
    let fleet_id = world.create_fleet(body.boss_id)?;
    if let Some(fleet) = world.fleets.get_mut(&fleet_id) {
        fleet.motd = body.motd;
    }
    Ok((StatusCode::CREATED, Json(json!(world.fleet(fleet_id)?))).into_response())
}

async fn disband(State(dev): State<DevEsi>, Path(id): Path<i64>) -> ControlResult {
    dev.world().disband(id)?;
    ok()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBody {
    character_id: i64,
    role: Option<Role>,
    wing_id: Option<i64>,
    squad_id: Option<i64>,
    /// A squad by name.
    squad: Option<String>,
}

async fn join(
    State(dev): State<DevEsi>,
    Path(fleet_id): Path<i64>,
    EsiJson(body): EsiJson<JoinBody>,
) -> ControlResult {
    let mut world = dev.world();
    let squad_id = match (&body.squad, body.squad_id) {
        (Some(name), _) => Some(
            world
                .fleet(fleet_id)?
                .wings
                .iter()
                .flat_map(|w| &w.squads)
                .find(|s| &s.name == name)
                .ok_or_else(|| DevError::bad_request(format!("no squad named {name:?}")))?
                .id,
        ),
        (None, id) => id,
    };
    world.join_fleet(
        fleet_id,
        body.character_id,
        body.role.unwrap_or(Role::SquadMember),
        body.wing_id,
        squad_id,
    )?;
    Ok(Json(json!(world.fleet(fleet_id)?)).into_response())
}

async fn remove_member(
    State(dev): State<DevEsi>,
    Path((fleet_id, character_id)): Path<(i64, i64)>,
) -> ControlResult {
    let mut world = dev.world();
    if world.fleet_of(character_id).map(|(f, _)| f.fleet_id) != Some(fleet_id) {
        return Err(DevError::not_found("Character is not in this fleet"));
    }
    world.leave_fleet(character_id);
    ok()
}

#[derive(Deserialize)]
struct CharacterRef {
    character_id: i64,
}

async fn set_boss(
    State(dev): State<DevEsi>,
    Path(fleet_id): Path<i64>,
    EsiJson(body): EsiJson<CharacterRef>,
) -> ControlResult {
    let mut world = dev.world();
    world.set_boss(fleet_id, body.character_id)?;
    Ok(Json(json!(world.fleet(fleet_id)?)).into_response())
}

async fn invites(State(dev): State<DevEsi>) -> ControlResult {
    Ok(Json(json!(dev.world().invites.values().collect::<Vec<_>>())).into_response())
}

async fn accept(State(dev): State<DevEsi>, Path(character_id): Path<i64>) -> ControlResult {
    let fleet_id = dev.world().accept_invite(character_id, None)?;
    Ok(Json(json!({"fleet_id": fleet_id})).into_response())
}

async fn decline(State(dev): State<DevEsi>, Path(character_id): Path<i64>) -> ControlResult {
    dev.world().decline_invite(character_id)?;
    ok()
}

async fn add_killmail(State(dev): State<DevEsi>, EsiJson(km): EsiJson<Killmail>) -> ControlResult {
    let km = dev.world().add_killmail(km)?;
    let (id, hash) = (km.killmail_id, km.killmail_hash);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "killmail_id": id,
            "killmail_hash": hash,
            // What players paste into the SRP form; the waitlist reads it from its own ESI URL.
            "link": format!("https://esi.evetech.net/latest/killmails/{id}/{hash}/"),
        })),
    )
        .into_response())
}

async fn add_journal(
    State(dev): State<DevEsi>,
    Path(corporation_id): Path<i64>,
    EsiJson(entry): EsiJson<JournalEntry>,
) -> ControlResult {
    let entry = dev
        .world()
        .add_journal_entry(corporation_id, entry.division, entry)?;
    Ok((StatusCode::CREATED, Json(json!(entry))).into_response())
}

async fn set_wallets(
    State(dev): State<DevEsi>,
    Path(corporation_id): Path<i64>,
    EsiJson(wallets): EsiJson<Vec<Wallet>>,
) -> ControlResult {
    if wallets.iter().any(|w| !(1..=7).contains(&w.division)) {
        return Err(DevError::bad_request("divisions are 1-7"));
    }
    let mut world = dev.world();
    world.corporation(corporation_id)?;
    let corporation = world
        .corporations
        .get_mut(&corporation_id)
        .expect("checked");
    for wallet in wallets {
        corporation
            .wallets
            .retain(|w| w.division != wallet.division);
        corporation.wallets.push(wallet);
    }
    Ok(Json(json!(world.wallets(corporation_id)?)).into_response())
}

async fn incursions(State(dev): State<DevEsi>) -> ControlResult {
    Ok(Json(json!(dev.world().incursions)).into_response())
}

async fn set_incursions(
    State(dev): State<DevEsi>,
    EsiJson(list): EsiJson<Vec<Incursion>>,
) -> ControlResult {
    dev.world().set_incursions(list);
    ok()
}

async fn wars(State(dev): State<DevEsi>) -> ControlResult {
    Ok(Json(json!(dev.world().wars.values().collect::<Vec<_>>())).into_response())
}

async fn set_wars(State(dev): State<DevEsi>, EsiJson(list): EsiJson<Vec<War>>) -> ControlResult {
    dev.world().set_wars(list);
    ok()
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
}

async fn events(State(dev): State<DevEsi>, Query(query): Query<EventsQuery>) -> ControlResult {
    let world = dev.world();
    let list: Vec<_> = world.events.iter().filter(|e| e.id > query.after).collect();
    Ok(Json(json!(list)).into_response())
}

async fn dashboard() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("dashboard.html")),
    )
        .into_response()
}
