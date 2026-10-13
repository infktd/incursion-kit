//! Joining the waitlist.
//!
//! - `POST /api/waitlist/xup` `{waitlist_id, character_id, eft, is_alt, messagexup, dna?}` (any
//!   logged-in account; every pilot must be the caller's own character): x-ups every fit of the
//!   EFT text (flown by `character_id`) plus every `{character_id, dna}`. Answers
//!   `{status: "ok", warnings}`.
//!
//! Each pilot is checked for bans and war, and their skills, implants and time in fleet are
//! loaded (ESI: skills and implants scopes). Each fit is run through the fit checker, replaces
//! the pilot's existing x-up of the same hull (on any waitlist) and is logged to `fit_history`.
//! Pilots at war cannot x-up the war-blocked categories (`rules.yaml`), and their other fits get
//! the war tag and are never auto-approved.
//!
//! Publishes `message` to every registered fleet's boss, then `waitlist_update`.

use std::collections::{BTreeSet, HashMap};

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::TypeId;
use wl_fitting::{DataSet, Fitting, PilotData, PilotSkills};

use super::notify;
use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(xup))
}

/// Most fits per request, and per account on one waitlist.
const MAX_X_PER_ACCOUNT: usize = 10;
/// Longest x-up message, in bytes.
const MAX_MESSAGE_LEN: usize = 100;

/// A fit given as DNA, flown by another of the caller's characters.
#[derive(Debug, Deserialize, ToSchema)]
struct DnaXup {
    /// One of the caller's own characters.
    character_id: i64,
    /// The fit as a ship DNA string.
    dna: String,
}

#[derive(Debug, Deserialize, ToSchema)]
struct XupRequest {
    waitlist_id: i64,
    /// One of the caller's own characters: the pilot of the EFT fits.
    character_id: i64,
    /// One or more fits in EFT format (may be empty when `dna` has fits).
    eft: String,
    /// X-up as an alt (invited into the `alt` category's squad).
    is_alt: bool,
    /// A message for the FCs, at most 100 bytes (may be empty).
    messagexup: String,
    /// More fits, each with its own pilot.
    #[serde(default)]
    dna: Vec<DnaXup>,
}

#[derive(Debug, Serialize, ToSchema)]
struct XupResponse {
    /// Always `ok`.
    #[schema(value_type = String, example = "ok")]
    status: &'static str,
    /// Notes for the pilot, e.g. that some categories are blocked while at war.
    warnings: Vec<String>,
}

/// What x-up needs to know about each pilot.
struct Pilot {
    time_in_fleet: i64,
    implants: Vec<TypeId>,
    skills: PilotSkills,
    at_war: bool,
}

/// X-up
///
/// Joins the waitlist with every fit of the EFT text (flown by `character_id`) plus every
/// `dna` fit. Any logged-in account may call it, but every pilot must be one of the caller's
/// own characters. Each pilot is checked for bans and war, and their implants and skills are
/// loaded from ESI (`esi-clones.read_implants.v1`, `esi-skills.read_skills.v1`; otherwise 401
/// `ESI error: missing ESI scope` / `ESI error: no ESI token found`). Each fit is run through
/// the fit checker (which sets its category, tags and auto-approval), replaces the pilot's
/// existing x-up of the same hull (on any waitlist) and is logged to the fit history. Pilots
/// at war cannot x-up the war-blocked categories (`rules.yaml`), and their other fits get the
/// war tag, are never auto-approved and add a warning. An account has at most 10 fits on a
/// waitlist. Nothing is saved if any fit fails.
///
/// Publishes `message` (`{"message": "New x-up in waitlist"}`) on the account topic of every
/// registered fleet's boss, then `waitlist_update` (`{"waitlist_id": …}`) on `waitlist`.
#[utoipa::path(
    post,
    path = "/api/waitlist/xup",
    tag = tags::WAITLIST,
    security(("session" = [])),
    request_body = XupRequest,
    responses(
        (status = 200, description = "The fits are on the waitlist.", body = XupResponse),
        (
            status = 400,
            description = "`No fits supplied`, `Too many fits` (more than 10 in the request or on the waitlist), `X-up message too long!`, `Waitlist is closed`, a ban (`You cannot join fleet as your character is banned.`, `… corporation is banned.`, `… alliance is banned.`, `You cannot join the waitlist as you are banned.`), `You cannot x-up <category> while your corporation or alliance is at war.`, the fit checker's first error, or `fit error: …` for a fit that can't be parsed or validated.",
            body = String,
            content_type = "text/plain",
        ),
    ),
)]
async fn xup(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<XupRequest>,
) -> ApiResult<Json<XupResponse>> {
    let data = state.data_store()?.get();
    let eve = data.eve();

    let mut xups: Vec<(i64, Fitting)> = Fitting::from_eft(&input.eft, eve)
        .map_err(fit_error)?
        .into_iter()
        .map(|fit| (input.character_id, fit))
        .collect();
    for dna in &input.dna {
        xups.push((
            dna.character_id,
            Fitting::from_dna(&dna.dna, eve).map_err(fit_error)?,
        ));
    }

    let warnings = xup_multi(
        &state,
        &data,
        &account,
        input.waitlist_id,
        xups,
        input.is_alt,
        &input.messagexup,
    )
    .await?;
    Ok(Json(XupResponse {
        status: "ok",
        warnings,
    }))
}

fn fit_error(err: wl_fitting::FitError) -> ApiError {
    ApiError::Fit(Box::new(err))
}

fn ban_message(category: &str) -> &'static str {
    match category {
        "Character" => "You cannot join fleet as your character is banned.",
        "Corporation" => "You cannot join fleet as your corporation is banned.",
        "Alliance" => "You cannot join fleet as your alliance is banned.",
        _ => "You cannot join the waitlist as you are banned.",
    }
}

/// Category names joined for messages: `A`, `A/B`.
fn category_names<'a>(data: &'a DataSet, ids: impl Iterator<Item = &'a str>) -> String {
    ids.map(|id| data.categories.name_of(id).unwrap_or(id))
        .collect::<Vec<_>>()
        .join("/")
}

/// The warning for pilots at war (with the default rules: "... You may still x-up Bastion/Other,
/// but Logistics is blocked.").
fn war_warning(data: &DataSet) -> String {
    let blocked = &data.rules.war.blocked_categories;
    let allowed = data
        .categories
        .categories
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| !blocked.iter().any(|b| b == id));
    format!(
        "Your corporation or alliance is at war. You may still x-up {}, but {} is blocked.",
        category_names(data, allowed),
        category_names(data, blocked.iter().map(String::as_str)),
    )
}

async fn time_in_fleet(state: &AppState, character_id: i64) -> ApiResult<i64> {
    let seconds: Option<i64> = sqlx::query_scalar(
        "SELECT CAST(SUM(last_seen - first_seen) AS SIGNED) FROM fleet_activity WHERE character_id = ?",
    )
    .bind(character_id)
    .fetch_one(state.db())
    .await?;
    Ok(seconds.unwrap_or(0))
}

async fn load_pilot(
    state: &AppState,
    account: &AuthenticatedAccount,
    character_id: i64,
) -> ApiResult<Pilot> {
    account
        .authorize_character(state.db(), character_id, None)
        .await?;

    if let Some(ban) = crate::ban::character_bans(state.db(), character_id)
        .await?
        .and_then(|bans| bans.into_iter().next())
    {
        let category = ban.entity.map(|e| e.category).unwrap_or_default();
        return Err(ApiError::bad_request(ban_message(&category)));
    }

    let esi = state.esi()?;
    let at_war = state
        .war_cache()
        .character_is_at_war(&state.affiliation()?, state.db(), character_id)
        .await?;
    let time_in_fleet = time_in_fleet(state, character_id).await?;
    let implants = esi.implants(character_id).await?;
    let skills = wl_workers::skills::load_skills(esi, state.db(), character_id).await?;
    Ok(Pilot {
        time_in_fleet,
        implants,
        skills: PilotSkills(skills),
        at_war,
    })
}

/// The id of the fitting row with this DNA, creating it if needed.
async fn dedup_dna(
    tx: &mut sqlx::MySqlConnection,
    hull: TypeId,
    dna: &str,
) -> Result<i64, sqlx::Error> {
    if let Some(id) = sqlx::query_scalar("SELECT id FROM fitting WHERE dna = ?")
        .bind(dna)
        .fetch_optional(&mut *tx)
        .await?
    {
        return Ok(id);
    }
    let result = sqlx::query("INSERT INTO fitting (dna, hull) VALUES (?, ?)")
        .bind(dna)
        .bind(hull)
        .execute(&mut *tx)
        .await?;
    Ok(last_insert_id(&result))
}

/// The id of the implant set row for these implants (sorted, `:`-joined), creating it if needed.
async fn dedup_implants(
    tx: &mut sqlx::MySqlConnection,
    implants: &[TypeId],
) -> Result<i64, sqlx::Error> {
    let mut implants = implants.to_vec();
    implants.sort_unstable();
    let implants = implants
        .iter()
        .map(TypeId::to_string)
        .collect::<Vec<_>>()
        .join(":");
    if let Some(id) = sqlx::query_scalar("SELECT id FROM implant_set WHERE implants = ?")
        .bind(&implants)
        .fetch_optional(&mut *tx)
        .await?
    {
        return Ok(id);
    }
    let result = sqlx::query("INSERT INTO implant_set (implants) VALUES (?)")
        .bind(&implants)
        .execute(&mut *tx)
        .await?;
    Ok(last_insert_id(&result))
}

fn last_insert_id(result: &sqlx::mysql::MySqlQueryResult) -> i64 {
    i64::try_from(result.last_insert_id()).unwrap_or(i64::MAX)
}

async fn xup_multi(
    state: &AppState,
    data: &DataSet,
    account: &AuthenticatedAccount,
    waitlist_id: i64,
    xups: Vec<(i64, Fitting)>,
    is_alt: bool,
    messagexup: &str,
) -> ApiResult<Vec<String>> {
    // "Now" is taken at the start so that the join time is fair.
    let now = chrono::Utc::now().timestamp();
    let mut warnings: Vec<String> = Vec::new();

    if xups.is_empty() {
        return Err(ApiError::bad_request("No fits supplied"));
    } else if xups.len() > MAX_X_PER_ACCOUNT {
        return Err(ApiError::bad_request("Too many fits"));
    }
    if messagexup.len() > MAX_MESSAGE_LEN {
        return Err(ApiError::bad_request("X-up message too long!"));
    }

    let open: Option<i64> =
        sqlx::query_scalar("SELECT id FROM waitlist WHERE id = ? AND is_open = 1")
            .bind(waitlist_id)
            .fetch_optional(state.db())
            .await?;
    if open.is_none() {
        return Err(ApiError::bad_request("Waitlist is closed"));
    }

    // Permissions, bans and ESI data for every pilot, before touching the database.
    let character_ids: BTreeSet<i64> = xups.iter().map(|(id, _)| *id).collect();
    let mut pilots = HashMap::new();
    for character_id in character_ids {
        pilots.insert(
            character_id,
            load_pilot(state, account, character_id).await?,
        );
    }
    let access_keys: BTreeSet<String> = account.access.iter().map(|k| k.to_string()).collect();

    let mut tx = state.db().begin().await?;

    let existing_entry: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM waitlist_entry WHERE account_id = ? AND waitlist_id = ?",
    )
    .bind(account.id)
    .bind(waitlist_id)
    .fetch_optional(&mut *tx)
    .await?;
    let entry_id = match existing_entry {
        Some(id) => id,
        None => {
            let result = sqlx::query(
                "INSERT INTO waitlist_entry (waitlist_id, account_id, joined_at) VALUES (?, ?, ?)",
            )
            .bind(waitlist_id)
            .bind(account.id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            last_insert_id(&result)
        }
    };

    // Spam protection: at most MAX_X_PER_ACCOUNT fits per entry.
    let existing_fits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM waitlist_entry_fit WHERE entry_id = ?")
            .bind(entry_id)
            .fetch_one(&mut *tx)
            .await?;
    if usize::try_from(existing_fits).unwrap_or(usize::MAX) + xups.len() > MAX_X_PER_ACCOUNT {
        return Err(ApiError::bad_request("Too many fits"));
    }

    let eve = data.eve();
    let war = &data.rules.war;
    for (character_id, fit) in xups {
        fit.validate(eve).map_err(fit_error)?;
        let pilot = &pilots[&character_id];

        let dna = fit.to_dna(eve).map_err(fit_error)?;
        let fit_id = dedup_dna(&mut tx, fit.hull, &dna).await?;
        let implant_set_id = dedup_implants(&mut tx, &pilot.implants).await?;

        // A pilot has one x-up per hull: replace the previous one (on any waitlist).
        let previous: Option<i64> = sqlx::query_scalar(
            "SELECT waitlist_entry_fit.id FROM waitlist_entry_fit
             JOIN fitting ON fit_id = fitting.id WHERE character_id = ? AND hull = ? LIMIT 1",
        )
        .bind(character_id)
        .bind(fit.hull)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(previous) = previous {
            sqlx::query("DELETE FROM waitlist_entry_fit WHERE id = ?")
                .bind(previous)
                .execute(&mut *tx)
                .await?;
        }

        let badges: Vec<String> = sqlx::query_scalar(
            "SELECT badge.name FROM badge
             JOIN badge_assignment ON badge.id = badge_assignment.badgeId
             WHERE badge_assignment.characterId = ?",
        )
        .bind(character_id)
        .fetch_all(&mut *tx)
        .await?;

        let pilot_data = PilotData {
            implants: &pilot.implants,
            time_in_fleet: pilot.time_in_fleet,
            skills: &pilot.skills,
            access_keys: &access_keys,
        };
        let checked = data
            .check_fit(&pilot_data, &fit, &badges)
            .map_err(fit_error)?;
        if let Some(error) = checked.errors.into_iter().next() {
            return Err(ApiError::BadRequest(error));
        }

        let mut approved = checked.approved;
        let mut tags = checked.tags;
        if pilot.at_war {
            if war.blocked_categories.contains(&checked.category) {
                return Err(ApiError::BadRequest(format!(
                    "You cannot x-up {} while your corporation or alliance is at war.",
                    category_names(data, std::iter::once(checked.category.as_str()))
                )));
            }
            approved = false;
            tags.push(war.tag.clone());
            let warning = war_warning(data);
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        let fit_analysis = checked
            .analysis
            .map(|analysis| serde_json::to_string(&analysis))
            .transpose()
            .map_err(ApiError::internal)?;

        sqlx::query(
            "INSERT INTO waitlist_entry_fit (character_id, entry_id, fit_id, category, approved,
                tags, implant_set_id, fit_analysis, cached_time_in_fleet, is_alt, messagexup)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(character_id)
        .bind(entry_id)
        .bind(fit_id)
        .bind(&checked.category)
        .bind(approved)
        .bind(tags.join(","))
        .bind(implant_set_id)
        .bind(fit_analysis)
        .bind(pilot.time_in_fleet)
        .bind(is_alt)
        .bind(messagexup)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO fit_history (character_id, fit_id, implant_set_id, logged_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(character_id)
        .bind(fit_id)
        .bind(implant_set_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    notify::waitlist_update_and_xup(state, waitlist_id).await?;
    Ok(warnings)
}
