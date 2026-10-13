//! Fleet invites.
//!
//! - `POST /api/waitlist/invite` `{id, character_id}` (`fleet-invite`, and `character_id` must be
//!   the caller's own character): invites the fit's pilot into the squad that the fleet bossed by
//!   `character_id` assigned to the fit's category (`alt` for alt x-ups), via ESI. 400
//!   `Fleet not configured` without such a squad. Text `OK`.
//!
//! Nothing changes in the database (the fit leaves the waitlist when the fleet updater sees the
//! pilot in fleet). Publishes `wakeup` (plain text) to `account;<entry account id>`; no
//! `waitlist_update`.

use axum::extract::State;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::TypeId;
use wl_live::{events, topics};

use crate::openapi::tags;
use crate::{ApiError, ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(invite))
}

/// The category whose squad alt x-ups are invited to (`categories.yaml` must define it).
const ALT_CATEGORY: &str = "alt";

#[derive(Deserialize, ToSchema)]
struct InviteRequest {
    /// The fit's x-up ID (`fits[].id` in `GET /api/waitlist`).
    id: i64,
    /// The inviting fleet boss: one of the caller's own characters.
    character_id: i64,
}

#[derive(sqlx::FromRow)]
struct XupRow {
    category: String,
    character_id: i64,
    is_alt: i8,
    account_id: i64,
    hull: TypeId,
}

/// Invite a pilot to fleet
///
/// Invites the fit's pilot into the squad that the fleet bossed by `character_id` registered
/// for the fit's category (the `alt` category's squad for alt x-ups), via ESI as
/// `character_id` (`esi-fleets.write_fleet.v1`). Requires `fleet-invite`, and `character_id`
/// must be one of the caller's own characters. The waitlist is not changed: the fit leaves it
/// when the fleet updater sees the pilot in fleet. Publishes `wakeup` (plain text `<FC> has
/// invited your <hull> to fleet.`, naming the caller's main) on the x-up account's topic; no
/// `waitlist_update`. ESI failures are passed on with ESI's status as `ESI error: <reason>`
/// (e.g. `ESI error: Character is already in a fleet`). An unknown fit ID is a 500 `database
/// error`.
#[utoipa::path(
    post,
    path = "/api/waitlist/invite",
    operation_id = "invite_to_fleet",
    tag = tags::WAITLIST,
    security(("session" = ["fleet-invite"])),
    request_body = InviteRequest,
    responses(
        (status = 200, description = "Invited.", body = String, content_type = "text/plain", example = "OK"),
        (status = 400, description = "`Fleet not configured` when `character_id` bosses no registered fleet with a squad for the category.", body = String, content_type = "text/plain"),
    ),
)]
async fn invite(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<InviteRequest>,
) -> ApiResult<&'static str> {
    account.require_access("fleet-invite")?;
    account
        .authorize_character(state.db(), input.character_id, None)
        .await?;

    let xup: XupRow = sqlx::query_as(
        "SELECT wef.category, wef.character_id, wef.is_alt, we.account_id, fitting.hull
         FROM waitlist_entry_fit wef
         JOIN waitlist_entry we ON wef.entry_id = we.id
         JOIN fitting ON wef.fit_id = fitting.id
         WHERE wef.id = ?",
    )
    .bind(input.id)
    .fetch_one(state.db())
    .await?;

    let category = if xup.is_alt > 0 {
        ALT_CATEGORY
    } else {
        xup.category.as_str()
    };
    let squad: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT fleet.id, fleet_squad.squad_id, fleet_squad.wing_id FROM fleet
         JOIN fleet_squad ON fleet.id = fleet_squad.fleet_id
         WHERE fleet.boss_id = ? AND fleet_squad.category = ?",
    )
    .bind(input.character_id)
    .bind(category)
    .fetch_optional(state.db())
    .await?;
    let Some((fleet_id, squad_id, wing_id)) = squad else {
        return Err(ApiError::bad_request("Fleet not configured"));
    };

    // Resolve everything the notification needs before inviting, so a lookup failure can't
    // turn an invite that already went out into an error response.
    // The legacy waitlist names the account's main, not the inviting character.
    let fc_name: String = sqlx::query_scalar("SELECT name FROM `character` WHERE id = ?")
        .bind(account.id)
        .fetch_one(state.db())
        .await?;
    let hull_name = state.eve_data()?.require(xup.hull)?.name.clone();
    state
        .esi()?
        .invite_to_fleet(
            input.character_id,
            fleet_id,
            xup.character_id,
            wing_id,
            squad_id,
        )
        .await?;

    state.live().publish(
        &topics::account(xup.account_id),
        events::WAKEUP,
        format!("{fc_name} has invited your {hull_name} to fleet."),
    );
    Ok("OK")
}
