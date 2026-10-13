//! A character's skills (ESI-backed).
//!
//! - `GET /api/skills?character_id=` (own characters, or `skill-view`): loads the skills from
//!   ESI (recording changes in `skill_current`/`skill_history`) and answers
//!   `{current, ids, categories, requirements}`: the active level of every skill `skills.yaml`
//!   mentions (0 if untrained), skill name → id, the skill categories and per-hull tiers.

use std::collections::BTreeMap;

use crate::extract::Query;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use wl_eve_data::{SkillLevel, TypeId};
use wl_fitting::{PilotSkills, SkillCategories, SkillRequirements, SkillTiers};

use crate::openapi::tags;
use crate::{ApiResult, AppState, AuthenticatedAccount};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(skills))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct CharacterQuery {
    /// The character whose skills to load.
    character_id: i64,
}

#[derive(Serialize, ToSchema)]
struct SkillsResponse {
    /// Skill type id → active level, for every skill the waitlist cares about (0 if untrained).
    #[schema(value_type = BTreeMap<i32, i8>)]
    current: BTreeMap<TypeId, SkillLevel>,
    /// Skill name → type id, for the same skills.
    #[schema(value_type = BTreeMap<String, i32>)]
    ids: BTreeMap<String, TypeId>,
    /// Skill category name → skill type ids (`skills.yaml`).
    #[schema(value_type = BTreeMap<String, Vec<i32>>)]
    categories: SkillCategories,
    /// Skill set (usually a hull name, or a variant such as `Armor Kronos`) → skill type id →
    /// required levels.
    #[schema(value_type = BTreeMap<String, BTreeMap<i32, SkillTiers>>)]
    requirements: SkillRequirements,
}

/// A character's skills
///
/// Loads the character's skills from ESI, recording changes in the skill history, and returns
/// the active level of every skill that `skills.yaml`, the doctrine fits or their prerequisites
/// mention, together with the skill names, categories and per-hull tiers from `skills.yaml`.
///
/// Requires login; the character must be the account's main or one of its alts, unless the
/// account has `skill-view` (else 401 `access denied`). ESI failures are passed through with
/// ESI's status as `ESI error: …`; a character without a token or the skills scope gets 401
/// `ESI error: no ESI token found` / `ESI error: missing ESI scope`.
#[utoipa::path(
    get,
    path = "/api/skills",
    operation_id = "character_skills",
    tag = tags::PILOT,
    security(("session" = [])),
    params(CharacterQuery),
    responses((status = 200, description = "The character's skills and the skill tables.", body = SkillsResponse)),
)]
async fn skills(
    State(state): State<AppState>,
    account: AuthenticatedAccount,
    Query(q): Query<CharacterQuery>,
) -> ApiResult<Json<SkillsResponse>> {
    account
        .authorize_character(state.db(), q.character_id, Some("skill-view"))
        .await?;
    let skills = PilotSkills(
        wl_workers::skills::load_skills(state.esi()?, state.db(), q.character_id).await?,
    );
    let data = state.data_store()?.get();
    let skill_data = &data.skills;
    Ok(Json(SkillsResponse {
        current: skill_data
            .relevant_skills
            .iter()
            .map(|&id| (id, skills.get(id)))
            .collect(),
        ids: skill_data.name_lookup.clone(),
        categories: skill_data.categories.clone(),
        requirements: skill_data.requirements.clone(),
    }))
}
