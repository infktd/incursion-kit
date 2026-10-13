//! Pilot info and alts.
//!
//! - `GET /api/pilot/info?character_id=` (own characters, or `pilot-view`): `{id, name, tags,
//!   active_bans}`. `tags` is `HQ-FC` or `TRAINEE` from the character's own role, then its badge
//!   names. An unknown character is a 500, as in the legacy waitlist.
//! - `GET /api/pilot/alts?character_id=` (`waitlist-tag:HQ-FC`): every character linked to it in
//!   either direction, by name.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::{Character, CharacterAndLevel};

use crate::auth::permissions;
use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(pilot_info))
        .routes(routes!(alts))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character to look up.
    character_id: i64,
}

/// Pilot information
///
/// The character's name, tags and active bans. `tags` starts with `HQ-FC` or `TRAINEE` when the
/// character's own role grants `waitlist-tag:HQ-FC` / `waitlist-tag:TRAINEE`, followed by the
/// names of its badges. `active_bans` are the bans affecting it (its own, else its
/// corporation's, else its alliance's), or `null` when there are none. Allowed for your own
/// characters (main or alt), or any character with `pilot-view`. An unknown character id is a
/// `500 database error`, as in the legacy waitlist.
#[utoipa::path(
    get,
    path = "/api/pilot/info",
    tag = tags::PILOT,
    security(("session" = [])),
    params(CharacterQuery),
    responses((status = 200, description = "The pilot.", body = CharacterAndLevel)),
)]
async fn pilot_info(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<CharacterQuery>,
) -> ApiResult<Json<CharacterAndLevel>> {
    account
        .authorize_character(state.db(), q.character_id, Some("pilot-view"))
        .await?;
    let (id, name): (i64, String) = sqlx::query_as("SELECT id, name FROM `character` WHERE id = ?")
        .bind(q.character_id)
        .fetch_one(state.db())
        .await?;

    let mut tags = Vec::new();
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM admin WHERE character_id = ?")
        .bind(id)
        .fetch_optional(state.db())
        .await?;
    if let Some(keys) = role.as_deref().and_then(permissions::access_keys) {
        if keys.contains("waitlist-tag:HQ-FC") {
            tags.push("HQ-FC".to_string());
        } else if keys.contains("waitlist-tag:TRAINEE") {
            tags.push("TRAINEE".to_string());
        }
    }
    let badges: Vec<String> = sqlx::query_scalar(
        "SELECT b.name FROM badge_assignment ba JOIN badge b ON b.id = ba.badgeId
         WHERE ba.characterId = ?",
    )
    .bind(id)
    .fetch_all(state.db())
    .await?;
    tags.extend(badges);

    let active_bans = crate::ban::character_bans(state.db(), id).await?;
    Ok(Json(CharacterAndLevel {
        id,
        name,
        tags,
        active_bans,
    }))
}

#[derive(sqlx::FromRow)]
struct CharacterRow {
    id: i64,
    name: String,
    corporation_id: Option<i64>,
}

/// Alts of a pilot
///
/// Every character linked to this one as an alt, in either direction (its alts, and the main it
/// is an alt of), sorted by name. Requires `waitlist-tag:HQ-FC`.
#[utoipa::path(
    get,
    path = "/api/pilot/alts",
    operation_id = "pilot_alts",
    tag = tags::PILOT,
    security(("session" = ["waitlist-tag:HQ-FC"])),
    params(CharacterQuery),
    responses((status = 200, description = "The linked characters.", body = Vec<Character>)),
)]
async fn alts(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<CharacterQuery>,
) -> ApiResult<Json<Vec<Character>>> {
    account.require_access("waitlist-tag:HQ-FC")?;
    let rows: Vec<CharacterRow> = sqlx::query_as(
        "SELECT c.id, c.name, c.corporation_id FROM `character` c
         JOIN alt_character alt ON (alt.alt_id = c.id OR alt.account_id = c.id)
         WHERE (alt.alt_id = ? OR alt.account_id = ?) AND c.id != ?
         ORDER BY c.name ASC",
    )
    .bind(q.character_id)
    .bind(q.character_id)
    .bind(q.character_id)
    .fetch_all(state.db())
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| Character {
                id: r.id,
                name: r.name,
                corporation_id: r.corporation_id,
            })
            .collect(),
    ))
}
