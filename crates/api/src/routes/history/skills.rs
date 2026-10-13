//! Skill history (DB table `skill_history`).
//!
//! - `GET /api/history/skills?character_id=` (self, own alt, or `skill-history-view`): skill
//!   changes (newest first), limited to the skills the waitlist cares about (`skills.yaml` plus
//!   what the doctrine fits need), and the name → id map of those skills.

use std::collections::BTreeMap;

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
    OpenApiRouter::new().routes(routes!(skill_history))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character whose skill history to show.
    character_id: i64,
}

/// One recorded skill level change.
#[derive(Serialize, ToSchema)]
struct SkillHistoryResponseLine {
    /// Skill type id.
    skill_id: TypeId,
    /// Level before the change (0 for a newly injected skill).
    old_level: i8,
    /// Level after the change.
    new_level: i8,
    /// When the change was noticed (Unix seconds).
    logged_at: i64,
    /// Skill name.
    name: String,
}

#[derive(Serialize, ToSchema)]
struct SkillHistoryResponse {
    /// Skill changes, newest first.
    history: Vec<SkillHistoryResponseLine>,
    /// Skill name → type id, for every skill the waitlist tracks.
    ids: BTreeMap<String, TypeId>,
}

#[derive(sqlx::FromRow)]
struct HistoryRow {
    skill_id: i32,
    old_level: i8,
    new_level: i8,
    logged_at: i64,
}

/// Skill history of a character
///
/// The character's recorded skill level changes, newest first, limited to the skills the
/// waitlist cares about (the community `skills.yaml` plus what the doctrine fits need), and the
/// name → id map of those skills. Allowed for your own characters (main or alt), or any
/// character with `skill-history-view`.
#[utoipa::path(
    get,
    path = "/api/history/skills",
    tag = tags::HISTORY,
    security(("session" = [])),
    params(CharacterQuery),
    responses((status = 200, description = "The skill history.", body = SkillHistoryResponse)),
)]
async fn skill_history(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(query): Query<CharacterQuery>,
) -> ApiResult<Json<SkillHistoryResponse>> {
    account
        .authorize_character(state.db(), query.character_id, Some("skill-history-view"))
        .await?;
    let data = state.data_store()?.get();
    let skills = &data.skills;

    let rows: Vec<HistoryRow> = sqlx::query_as(
        "SELECT skill_id, old_level, new_level, logged_at FROM skill_history
         WHERE character_id = ? ORDER BY id DESC",
    )
    .bind(query.character_id)
    .fetch_all(state.db())
    .await?;
    let history = rows
        .into_iter()
        .filter(|row| skills.relevant_skills.contains(&row.skill_id))
        .filter_map(|row| {
            Some(SkillHistoryResponseLine {
                name: skills.id_lookup.get(&row.skill_id)?.clone(),
                skill_id: row.skill_id,
                old_level: row.old_level,
                new_level: row.new_level,
                logged_at: row.logged_at,
            })
        })
        .collect();

    Ok(Json(SkillHistoryResponse {
        history,
        ids: skills.name_lookup.clone(),
    }))
}
