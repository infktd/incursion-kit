//! Open an in-game window via ESI.
//!
//! - `POST /api/open_window` `{character_id, target_id}` (`character_id` must be the caller's
//!   own character, with the open-window scope): shows `target_id`'s info window in that
//!   character's client. 200 with an empty body.

use axum::extract::State;
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount, JsonBody};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(open_window))
}

#[derive(Deserialize, ToSchema)]
struct OpenWindowRequest {
    /// One of the caller's own characters (main or alt), logged in to the game.
    character_id: i64,
    /// The character, corporation or alliance whose information window to open.
    target_id: i64,
}

/// Open an information window
///
/// Shows `target_id`'s information window in the game client of `character_id`, which must be
/// one of the caller's own characters (main or alt) and have the `esi-ui.open_window.v1`
/// scope (otherwise 401 `ESI error: missing ESI scope` / `ESI error: no ESI token found`). ESI
/// failures are passed on with ESI's status as `ESI error: <reason>`.
#[utoipa::path(
    post,
    path = "/api/open_window",
    tag = tags::FLEET,
    security(("session" = [])),
    request_body = OpenWindowRequest,
    responses((status = 200, description = "The window was opened (empty body).")),
)]
async fn open_window(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    JsonBody(input): JsonBody<OpenWindowRequest>,
) -> ApiResult<()> {
    account
        .authorize_character(state.db(), input.character_id, None)
        .await?;
    state
        .esi()?
        .open_information_window(input.character_id, input.target_id)
        .await?;
    Ok(())
}
