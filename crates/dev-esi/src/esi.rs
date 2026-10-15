//! The ESI endpoints the waitlist calls, at ESI's paths, with its JSON shapes, status codes and
//! error bodies (`{"error": "..."}`).
//!
//! Authenticated endpoints need a dev-esi access token (`Authorization: Bearer …`) carrying the
//! endpoint's scope (403 `token not valid for scope` otherwise) and, for character endpoints,
//! issued to that character. Fleet endpoints answer only the fleet boss, as ESI does.
//!
//! GET responses carry an `ETag` and answer `If-None-Match` with 304; `GET /incursions` also has
//! `Expires` and `Last-Modified` (the incursion tracker schedules its polls from them). Nothing
//! else sets `Expires`, so clients revalidate every time and see changes at once.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts, Path, Query, RawQuery, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DevEsi;
use crate::seed::SeedTime;
use crate::sso::character_of;
use crate::world::{DevError, Role, World};

pub fn routes() -> Router<DevEsi> {
    Router::new()
        .route("/characters/affiliation", post(affiliation))
        .route("/characters/{character_id}", get(character))
        .route("/characters/{character_id}/skills", get(skills))
        .route("/characters/{character_id}/implants", get(implants))
        .route("/characters/{character_id}/fleet", get(character_fleet))
        .route("/characters/{character_id}/search", get(search))
        .route("/corporations/{corporation_id}", get(corporation))
        .route("/corporations/{corporation_id}/wallets", get(wallets))
        .route(
            "/corporations/{corporation_id}/wallets/{division}/journal",
            get(journal),
        )
        .route("/alliances/{alliance_id}", get(alliance))
        .route("/universe/names", post(names))
        .route("/universe/types/{type_id}", get(universe_type))
        .route(
            "/universe/constellations/{constellation_id}",
            get(constellation),
        )
        .route("/universe/systems/{system_id}", get(system))
        .route("/fleets/{fleet_id}", get(fleet).put(update_fleet))
        .route("/fleets/{fleet_id}/wings", get(fleet_wings))
        .route(
            "/fleets/{fleet_id}/members",
            get(fleet_members).post(invite),
        )
        .route(
            "/fleets/{fleet_id}/members/{member_id}",
            axum::routing::delete(kick).put(move_member),
        )
        .route("/ui/openwindow/information", post(open_information))
        .route("/killmails/{killmail_id}/{killmail_hash}", get(killmail))
        .route("/wars", get(wars))
        .route("/wars/{war_id}", get(war))
        .route("/incursions", get(incursions))
}

/// ESI's error response.
pub fn esi_error(status: u16, message: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "error": message }))).into_response()
}

impl IntoResponse for DevError {
    fn into_response(self) -> Response {
        esi_error(self.status, &self.message)
    }
}

type EsiResult = Result<Response, DevError>;

/// Path parameters, with ESI's JSON error on a bad value.
pub struct EsiPath<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for EsiPath<T> {
    type Rejection = DevError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| EsiPath(value))
            .map_err(|e: PathRejection| DevError::new(400, e.body_text()))
    }
}

/// A JSON body, with ESI's JSON error when it doesn't parse.
pub struct EsiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for EsiJson<T> {
    type Rejection = DevError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| EsiJson(value))
            .map_err(|e: JsonRejection| DevError::new(400, e.body_text()))
    }
}

/// A JSON response with an `ETag`; 304 when the request's `If-None-Match` already has it.
fn cached_json<T: Serialize>(request: &HeaderMap, value: &T) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    let etag = format!("\"{:016x}\"", hasher.finish());
    let matches = request
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|t| t.trim().trim_start_matches("W/") == etag)
        });
    if matches {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    (
        [
            (header::ETAG, etag),
            (header::CONTENT_TYPE, "application/json".to_string()),
        ],
        body,
    )
        .into_response()
}

/// `value` as JSON without one of its fields (dev-esi's own bookkeeping).
fn without<T: Serialize>(value: &T, field: &str) -> Value {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    if let Value::Object(map) = &mut value {
        map.remove(field);
    }
    value
}

fn http_date(time: DateTime<Utc>) -> String {
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

/// The caller of an authenticated endpoint.
struct Caller {
    character_id: i64,
}

/// Check the bearer token: signed by dev-esi, unexpired, with `scope`.
fn authenticate(dev: &DevEsi, headers: &HeaderMap, scope: &str) -> Result<Caller, DevError> {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return Err(DevError::new(401, "authorization not provided"));
    };
    let Some(claims) = dev.signer().verify(token.trim()) else {
        return Err(DevError::new(401, "token is not valid"));
    };
    if claims["exp"]
        .as_i64()
        .is_none_or(|exp| exp < Utc::now().timestamp())
    {
        return Err(DevError::new(401, "token is expired"));
    }
    let Some(character_id) = character_of(&claims) else {
        return Err(DevError::new(401, "token is not valid"));
    };
    let has_scope = match &claims["scp"] {
        Value::String(s) => s == scope,
        Value::Array(list) => list.iter().any(|s| s == scope),
        _ => false,
    };
    if !has_scope {
        return Err(DevError::new(403, "token not valid for scope"));
    }
    Ok(Caller { character_id })
}

/// Like [`authenticate`], for an endpoint about `character_id` itself.
fn authenticate_as(
    dev: &DevEsi,
    headers: &HeaderMap,
    scope: &str,
    character_id: i64,
) -> Result<Caller, DevError> {
    let caller = authenticate(dev, headers, scope)?;
    if caller.character_id != character_id {
        return Err(DevError::new(403, "token not valid for this character"));
    }
    Ok(caller)
}

// --- public character, corporation and alliance data ------------------------------------------

async fn character(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    let c = world.character(id)?;
    let mut body = json!({
        "achievement_score": 0,
        "birthday": c.birthday,
        "bloodline_id": 1,
        "corporation_id": c.corporation_id,
        "description": "",
        "gender": if id % 2 == 0 { "female" } else { "male" },
        "name": c.name,
        "race_id": 1,
        "security_status": c.security_status,
    });
    if let Some(alliance) = world.alliance_of(c) {
        body["alliance_id"] = json!(alliance);
    }
    Ok(cached_json(&headers, &body))
}

async fn corporation(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    let corp = world.corporation(id)?;
    let members = world
        .characters
        .values()
        .filter(|c| c.corporation_id == id)
        .count();
    let ceo = corp.ceo_id.or_else(|| {
        world
            .characters
            .values()
            .find(|c| c.corporation_id == id)
            .map(|c| c.id)
    });
    let mut body = json!({
        "date_founded": "2020-01-01T00:00:00Z",
        "description": corp.description,
        "friendly_fire": "illegal",
        "home_station_id": 60003760,
        "member_count": members,
        "name": corp.name,
        "shares": 1000,
        "state": "active",
        "tax_rates": {"isk": 0.1, "loyalty_point": 0.1},
        "ticker": corp.ticker,
        "type": "player_owned",
        "war_eligible": true,
    });
    if let Some(alliance) = corp.alliance_id {
        body["alliance_id"] = json!(alliance);
    }
    if let Some(ceo) = ceo {
        body["ceo_id"] = json!(ceo);
        body["creator_id"] = json!(ceo);
    }
    Ok(cached_json(&headers, &body))
}

async fn alliance(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    let alliance = world
        .alliances
        .get(&id)
        .ok_or_else(|| DevError::not_found("Alliance not found"))?;
    let executor = alliance.executor_corporation_id.or_else(|| {
        world
            .corporations
            .values()
            .find(|c| c.alliance_id == Some(id))
            .map(|c| c.id)
    });
    let creator = executor
        .and_then(|corp| world.corporations.get(&corp))
        .and_then(|c| c.ceo_id)
        .unwrap_or(0);
    let mut body = json!({
        "creator_corporation_id": executor.unwrap_or(0),
        "creator_id": creator,
        "date_founded": alliance.date_founded,
        "name": alliance.name,
        "ticker": alliance.ticker,
    });
    if let Some(executor) = executor {
        body["executor_corporation_id"] = json!(executor);
    }
    Ok(cached_json(&headers, &body))
}

/// `POST /characters/affiliation`. Unknown IDs are left out.
async fn affiliation(State(dev): State<DevEsi>, EsiJson(ids): EsiJson<Vec<i64>>) -> EsiResult {
    if ids.len() > 1000 {
        return Err(DevError::new(400, "Too many IDs (at most 1000)"));
    }
    let world = dev.world();
    let found: Vec<Value> = ids
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|id| world.characters.get(id))
        .map(|c| {
            let mut a = json!({"character_id": c.id, "corporation_id": c.corporation_id});
            if let Some(alliance) = world.alliance_of(c) {
                a["alliance_id"] = json!(alliance);
            }
            a
        })
        .collect();
    Ok(Json(found).into_response())
}

/// `POST /universe/names`: fails as a whole (404) when any ID is unknown, like ESI.
async fn names(State(dev): State<DevEsi>, EsiJson(ids): EsiJson<Vec<i64>>) -> EsiResult {
    if ids.len() > 1000 {
        return Err(DevError::new(400, "Too many IDs (at most 1000)"));
    }
    let world = dev.world();
    let mut found = Vec::new();
    for id in ids.iter().collect::<BTreeSet<_>>() {
        let Some((name, category)) = world.name_of(*id, dev.universe()) else {
            return Err(DevError::new(
                404,
                "Ensure all IDs are valid before resolving.",
            ));
        };
        found.push(json!({"category": category, "id": id, "name": name}));
    }
    Ok(Json(found).into_response())
}

async fn universe_type(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    let body = if let Some(t) = dev.universe().types.get(&id) {
        json!({"description": "", "group_id": t.group_id, "name": t.name,
               "published": t.published, "type_id": id})
    } else if let Some(n) = world
        .names
        .get(&id)
        .filter(|n| n.category == "inventory_type")
    {
        json!({"description": "", "group_id": 0, "name": n.name, "published": true,
               "type_id": id})
    } else {
        return Err(DevError::new(404, "Type not found"));
    };
    Ok(cached_json(&headers, &body))
}

async fn constellation(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let c = dev
        .universe()
        .constellations
        .get(&id)
        .ok_or_else(|| DevError::new(404, "Constellation not found"))?;
    let body = json!({
        "constellation_id": id,
        "name": c.name,
        "position": {"x": 0.0, "y": 0.0, "z": 0.0},
        "region_id": c.region_id,
        "systems": c.systems,
    });
    Ok(cached_json(&headers, &body))
}

async fn system(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let s = dev
        .universe()
        .systems
        .get(&id)
        .ok_or_else(|| DevError::new(404, "Solar system not found"))?;
    let body = json!({
        "constellation_id": s.constellation_id,
        "name": s.name,
        "planets": [],
        "position": {"x": 0.0, "y": 0.0, "z": 0.0},
        "security_status": s.security,
        "stargates": [],
        "stations": s.stations,
        "system_id": id,
    });
    Ok(cached_json(&headers, &body))
}

// --- character data (authenticated) -----------------------------------------------------------

/// Skill points of a skill of `rank` at `level`: 250 × rank × √32^(level−1).
fn skill_points(rank: f64, level: i64) -> i64 {
    if level <= 0 {
        return 0;
    }
    (250.0 * rank * 32f64.sqrt().powi(i32::try_from(level - 1).unwrap_or(0))).round() as i64
}

async fn skills(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    authenticate_as(&dev, &headers, "esi-skills.read_skills.v1", id)?;
    let world = dev.world();
    let character = world.character(id)?;
    let mut total = 0;
    let skills: Vec<Value> = world
        .skills_of(character)
        .into_iter()
        .map(|(skill_id, level)| {
            let rank = dev
                .universe()
                .types
                .get(&skill_id)
                .and_then(|t| t.rank)
                .unwrap_or(1.0);
            let sp = skill_points(rank, level);
            total += sp;
            json!({"active_skill_level": level, "skill_id": skill_id,
                   "skillpoints_in_skill": sp, "trained_skill_level": level})
        })
        .collect();
    Ok(cached_json(
        &headers,
        &json!({"skills": skills, "total_sp": total, "unallocated_sp": 0}),
    ))
}

async fn implants(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    authenticate_as(&dev, &headers, "esi-clones.read_implants.v1", id)?;
    let world = dev.world();
    Ok(cached_json(&headers, &world.character(id)?.implants))
}

async fn character_fleet(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    authenticate_as(&dev, &headers, "esi-fleets.read_fleet.v1", id)?;
    let world = dev.world();
    world.character(id)?;
    let (fleet, member) = world
        .fleet_of(id)
        .ok_or_else(|| DevError::new(404, "Character is not in a fleet"))?;
    Ok(cached_json(
        &headers,
        &json!({
            "fleet_boss_id": fleet.boss_id,
            "fleet_id": fleet.fleet_id,
            "role": member.role,
            "squad_id": member.squad_id,
            "wing_id": member.wing_id,
        }),
    ))
}

/// `GET /characters/{id}/search?categories=…&search=…&strict=…`: characters, corporations and
/// alliances by name (case-insensitive substring, or the exact name when strict).
async fn search(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
    RawQuery(query): RawQuery,
) -> EsiResult {
    authenticate_as(&dev, &headers, "esi-search.search_structures.v1", id)?;
    let mut categories = Vec::new();
    let mut search = String::new();
    let mut strict = false;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "categories" => categories.extend(value.split(',').map(str::to_string)),
            "search" => search = value.into_owned(),
            "strict" => strict = value == "true",
            _ => {}
        }
    }
    if categories.is_empty() {
        return Err(DevError::new(400, "categories is required"));
    }
    if search.chars().count() < 3 {
        return Err(DevError::new(400, "search must be at least 3 characters"));
    }
    let needle = search.to_lowercase();
    let hit = |name: &str| {
        if strict {
            name.to_lowercase() == needle
        } else {
            name.to_lowercase().contains(&needle)
        }
    };
    let world = dev.world();
    let mut body = serde_json::Map::new();
    for category in categories {
        let ids: Vec<i64> = match category.as_str() {
            "character" => world
                .characters
                .values()
                .filter(|c| hit(&c.name))
                .map(|c| c.id)
                .collect(),
            "corporation" => world
                .corporations
                .values()
                .filter(|c| hit(&c.name) || hit(&c.ticker))
                .map(|c| c.id)
                .collect(),
            "alliance" => world
                .alliances
                .values()
                .filter(|a| hit(&a.name) || hit(&a.ticker))
                .map(|a| a.id)
                .collect(),
            _ => Vec::new(),
        };
        if !ids.is_empty() {
            body.insert(category, json!(ids));
        }
    }
    Ok(cached_json(&headers, &Value::Object(body)))
}

// --- corporation wallets ---------------------------------------------------------------------

const WALLET_SCOPE: &str = "esi-wallet.read_corporation_wallets.v1";

/// The caller must belong to the corporation (dev-esi gives every member the accountant role).
fn wallet_reader(
    dev: &DevEsi,
    world: &World,
    headers: &HeaderMap,
    corporation_id: i64,
) -> Result<(), DevError> {
    let caller = authenticate(dev, headers, WALLET_SCOPE)?;
    world.corporation(corporation_id)?;
    if world
        .characters
        .get(&caller.character_id)
        .is_none_or(|c| c.corporation_id != corporation_id)
    {
        return Err(DevError::new(
            403,
            "Character does not have required role(s)",
        ));
    }
    Ok(())
}

async fn wallets(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(corporation_id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    wallet_reader(&dev, &world, &headers, corporation_id)?;
    Ok(cached_json(&headers, &world.wallets(corporation_id)?))
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<i64>,
}

/// Journal entries per page, as ESI pages them.
const JOURNAL_PAGE: usize = 2500;

async fn journal(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath((corporation_id, division)): EsiPath<(i64, i64)>,
    Query(query): Query<PageQuery>,
) -> EsiResult {
    let world = dev.world();
    wallet_reader(&dev, &world, &headers, corporation_id)?;
    if !(1..=7).contains(&division) {
        return Err(DevError::new(400, "division must be between 1 and 7"));
    }
    let page = query.page.unwrap_or(1);
    if page < 1 {
        return Err(DevError::new(400, "page must be at least 1"));
    }
    let mut entries: Vec<_> = world.corporations[&corporation_id]
        .journal
        .iter()
        .filter(|e| e.division == division)
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse((e.date, e.id)));
    let pages = entries.len().div_ceil(JOURNAL_PAGE).max(1);
    let start = usize::try_from(page - 1)
        .unwrap_or(usize::MAX)
        .saturating_mul(JOURNAL_PAGE);
    let slice: Vec<Value> = entries
        .into_iter()
        .skip(start)
        .take(JOURNAL_PAGE)
        .map(|e| without(e, "division"))
        .collect();
    let mut response = cached_json(&headers, &slice);
    response.headers_mut().insert(
        "x-pages",
        HeaderValue::from_str(&pages.to_string()).expect("digits"),
    );
    Ok(response)
}

// --- fleets ----------------------------------------------------------------------------------

const READ_FLEET: &str = "esi-fleets.read_fleet.v1";
const WRITE_FLEET: &str = "esi-fleets.write_fleet.v1";

async fn fleet(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(fleet_id): EsiPath<i64>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, READ_FLEET)?;
    let world = dev.world();
    let fleet = world.fleet_for_boss(fleet_id, caller.character_id)?;
    Ok(cached_json(
        &headers,
        &json!({
            "is_free_move": fleet.is_free_move,
            "is_registered": fleet.is_registered,
            "is_voice_enabled": fleet.is_voice_enabled,
            "motd": fleet.motd,
        }),
    ))
}

#[derive(Deserialize)]
struct FleetUpdate {
    motd: Option<String>,
    is_free_move: Option<bool>,
}

async fn update_fleet(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(fleet_id): EsiPath<i64>,
    EsiJson(body): EsiJson<FleetUpdate>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, WRITE_FLEET)?;
    dev.world()
        .update_fleet(fleet_id, caller.character_id, body.motd, body.is_free_move)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn fleet_wings(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(fleet_id): EsiPath<i64>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, READ_FLEET)?;
    let world = dev.world();
    let fleet = world.fleet_for_boss(fleet_id, caller.character_id)?;
    Ok(cached_json(&headers, &fleet.wings))
}

async fn fleet_members(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(fleet_id): EsiPath<i64>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, READ_FLEET)?;
    let world = dev.world();
    let fleet = world.fleet_for_boss(fleet_id, caller.character_id)?;
    let members: Vec<Value> = fleet
        .members
        .iter()
        .map(|m| {
            let c = world.characters.get(&m.character_id);
            let mut member = json!({
                "character_id": m.character_id,
                "join_time": m.join_time,
                "role": m.role,
                "role_name": World::role_name(fleet, m),
                "ship_type_id": c.map_or(670, |c| c.ship_type_id),
                "solar_system_id": c.map_or(30000142, |c| c.solar_system_id),
                "squad_id": m.squad_id,
                "takes_fleet_warp": m.takes_fleet_warp,
                "wing_id": m.wing_id,
            });
            if let Some(station) = c.and_then(|c| c.station_id) {
                member["station_id"] = json!(station);
            }
            member
        })
        .collect();
    Ok(cached_json(&headers, &members))
}

#[derive(Deserialize)]
struct InviteBody {
    character_id: i64,
    role: Role,
    squad_id: Option<i64>,
    wing_id: Option<i64>,
}

/// `POST /fleets/{id}/members`: the pilot accepts by itself after the configured delay when it
/// has `auto_accept_invites`; otherwise the invite waits for the control API.
async fn invite(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(fleet_id): EsiPath<i64>,
    EsiJson(body): EsiJson<InviteBody>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, WRITE_FLEET)?;
    let (invite, auto_accept) = dev.world().invite(
        fleet_id,
        caller.character_id,
        body.character_id,
        body.role,
        body.wing_id,
        body.squad_id,
    )?;
    if auto_accept {
        dev.schedule_accept(invite.character_id, invite.id);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn kick(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath((fleet_id, member_id)): EsiPath<(i64, i64)>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, WRITE_FLEET)?;
    dev.world().kick(fleet_id, caller.character_id, member_id)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct MoveBody {
    role: Role,
    squad_id: Option<i64>,
    wing_id: Option<i64>,
}

async fn move_member(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath((fleet_id, member_id)): EsiPath<(i64, i64)>,
    EsiJson(body): EsiJson<MoveBody>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, WRITE_FLEET)?;
    dev.world().move_member(
        fleet_id,
        caller.character_id,
        member_id,
        body.role,
        body.wing_id,
        body.squad_id,
    )?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- UI ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct WindowQuery {
    target_id: i64,
}

/// `POST /ui/openwindow/information`: recorded as an `open_window` event.
async fn open_information(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    Query(query): Query<WindowQuery>,
) -> EsiResult {
    let caller = authenticate(&dev, &headers, "esi-ui.open_window.v1")?;
    let mut world = dev.world();
    let target = world
        .name_of(query.target_id, dev.universe())
        .map_or_else(|| query.target_id.to_string(), |(name, _)| name);
    let detail = format!(
        "show-info window for {target} ({}) opened in {}'s client",
        query.target_id,
        world.character_name(caller.character_id)
    );
    world.event("open_window", Some(caller.character_id), None, detail);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- killmails, wars, incursions --------------------------------------------------------------

async fn killmail(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath((id, hash)): EsiPath<(i64, String)>,
) -> EsiResult {
    let world = dev.world();
    let km = world
        .killmails
        .get(&id)
        .filter(|k| k.killmail_hash == hash)
        .ok_or_else(|| DevError::new(422, "Invalid killmail_id and/or killmail_hash"))?;
    Ok(cached_json(&headers, &without(km, "killmail_hash")))
}

#[derive(Deserialize)]
struct WarsQuery {
    max_war_id: Option<i64>,
}

async fn wars(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    Query(query): Query<WarsQuery>,
) -> EsiResult {
    let world = dev.world();
    let ids: Vec<i64> = world
        .wars
        .keys()
        .rev()
        .filter(|id| query.max_war_id.is_none_or(|max| **id < max))
        .take(2000)
        .copied()
        .collect();
    Ok(cached_json(&headers, &ids))
}

async fn war(
    State(dev): State<DevEsi>,
    headers: HeaderMap,
    EsiPath(id): EsiPath<i64>,
) -> EsiResult {
    let world = dev.world();
    let war = world
        .wars
        .get(&id)
        .ok_or_else(|| DevError::new(404, "War not found"))?;
    Ok(cached_json(&headers, war))
}

async fn incursions(State(dev): State<DevEsi>, headers: HeaderMap) -> EsiResult {
    let world = dev.world();
    let mut response = cached_json(&headers, &world.incursions);
    let now = SeedTime::now().0;
    let expires = now + chrono::Duration::seconds(world.settings.incursions_cache_secs as i64);
    for (name, value) in [
        (header::DATE, http_date(now)),
        (header::EXPIRES, http_date(expires)),
        (header::LAST_MODIFIED, http_date(world.incursions_modified)),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_str(&value).expect("an HTTP date"));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_points_follow_the_training_curve() {
        assert_eq!(skill_points(1.0, 1), 250);
        assert_eq!(skill_points(1.0, 5), 256_000);
        assert_eq!(skill_points(8.0, 5), 2_048_000);
        assert_eq!(skill_points(1.0, 0), 0);
    }

    #[test]
    fn http_dates() {
        let t = DateTime::parse_from_rfc3339("2026-10-11T06:35:20Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(http_date(t), "Sun, 11 Oct 2026 06:35:20 GMT");
    }
}
