//! A character's implants (ESI-backed).
//!
//! - `GET /api/implants?character_id=` (own characters only): `{implants: [type id]}`.

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_core::types::TypeId;

use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(implants))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character whose implants to load.
    character_id: i64,
}

#[derive(Serialize, ToSchema)]
struct ImplantsResponse {
    /// Type ids of the implants in the character's active clone.
    #[schema(value_type = Vec<i32>)]
    implants: Vec<TypeId>,
}

/// A character's implants
///
/// The implants of the character's active clone, from ESI.
///
/// Requires login; only for the account's own characters (main or alts), with no override
/// permission (else 401 `access denied`). ESI failures are passed through with ESI's status as
/// `ESI error: …`; a character without a token or the implants scope gets 401
/// `ESI error: no ESI token found` / `ESI error: missing ESI scope`.
#[utoipa::path(
    get,
    path = "/api/implants",
    operation_id = "character_implants",
    tag = tags::DOCTRINE,
    security(("session" = [])),
    params(CharacterQuery),
    responses((status = 200, description = "The implants.", body = ImplantsResponse)),
)]
async fn implants(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<CharacterQuery>,
) -> ApiResult<Json<ImplantsResponse>> {
    account
        .authorize_character(state.db(), q.character_id, None)
        .await?;
    let implants = state.esi()?.implants(q.character_id).await?;
    Ok(Json(ImplantsResponse { implants }))
}
