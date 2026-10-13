//! Loading a character's skills from ESI into `skill_current`, logging changes to
//! `skill_history`. Used by the skills page, x-up and the skill updater.

use std::collections::HashMap;

use wl_db::Db;
use wl_esi::Esi;
use wl_eve_data::{SkillLevel, TypeId};

use crate::Result;

/// Fetch `character_id`'s skills (needs `esi-skills.read_skills.v1`) and store them.
///
/// Returns the **active** level of every skill (what an Alpha clone can use). The database keeps
/// the **trained** level: a changed level is logged to `skill_history` (from 0 for a newly
/// trained skill, unless this is the first time the character's skills are stored).
pub async fn load_skills(
    esi: &Esi,
    db: &Db,
    character_id: i64,
) -> Result<HashMap<TypeId, SkillLevel>> {
    let skills = esi.skills(character_id).await?;

    let known: HashMap<TypeId, SkillLevel> = sqlx::query_as::<_, (TypeId, SkillLevel)>(
        "SELECT skill_id, level FROM skill_current WHERE character_id = ?",
    )
    .bind(character_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();

    let now = chrono::Utc::now().timestamp();
    let mut tx = db.begin().await?;
    let mut active = HashMap::with_capacity(skills.len());
    for skill in skills {
        active.insert(skill.skill_id, skill.active_skill_level);

        let old_level = match known.get(&skill.skill_id) {
            Some(&level) if level == skill.trained_skill_level => continue,
            Some(&level) => Some(level),
            None if known.is_empty() => None,
            None => Some(0),
        };
        if let Some(old_level) = old_level {
            sqlx::query(
                "INSERT INTO skill_history (character_id, skill_id, old_level, new_level, logged_at) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(character_id)
            .bind(skill.skill_id)
            .bind(old_level)
            .bind(skill.trained_skill_level)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("REPLACE INTO skill_current (character_id, skill_id, level) VALUES (?, ?, ?)")
            .bind(character_id)
            .bind(skill.skill_id)
            .bind(skill.trained_skill_level)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(active)
}
