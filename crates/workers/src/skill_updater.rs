//! Skill updater: refreshes the skills of every character with a stored refresh token, spread
//! evenly (in random order) over `skill_updater.runtime` seconds, then starts over after a
//! minute.

use std::time::Duration;

use rand::seq::SliceRandom as _;
use tokio_util::sync::CancellationToken;
use wl_db::Db;
use wl_esi::Esi;

use crate::runner::{self, Backoff};
use crate::{Error, Result};

/// Pause between two full passes.
pub const PAUSE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct SkillUpdater {
    pub db: Db,
    pub esi: Esi,
    /// Seconds over which one pass is spread (`skill_updater.runtime`).
    pub runtime: f64,
}

impl SkillUpdater {
    pub async fn run(self, cancel: CancellationToken) {
        runner::run(
            "skill_updater",
            cancel,
            PAUSE,
            Backoff::new(PAUSE, Duration::from_secs(600)),
            || self.run_once(),
        )
        .await;
    }

    /// One pass over every character. ESI failures for a character are logged and skipped; a
    /// database failure ends the pass.
    pub async fn run_once(&self) -> Result<()> {
        let mut characters: Vec<i64> = sqlx::query_scalar("SELECT character_id FROM refresh_token")
            .fetch_all(&self.db)
            .await?;
        if characters.is_empty() {
            return Ok(());
        }
        characters.shuffle(&mut rand::rng());
        let per_character = per_character_delay(self.runtime, characters.len());

        for character_id in characters {
            match crate::skills::load_skills(&self.esi, &self.db, character_id).await {
                Ok(_) => {}
                Err(Error::Esi(e)) => {
                    tracing::debug!(character_id, error = %e, "skipping skills of character");
                }
                Err(e) => return Err(e),
            }
            tokio::time::sleep(per_character).await;
        }
        Ok(())
    }
}

/// `runtime / count` seconds, sanitised (never negative, NaN or infinite).
fn per_character_delay(runtime: f64, count: usize) -> Duration {
    let seconds = runtime / count.max(1) as f64;
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_spread_and_sane() {
        assert_eq!(per_character_delay(100.0, 4), Duration::from_secs(25));
        assert_eq!(per_character_delay(100.0, 0), Duration::from_secs(100));
        assert_eq!(per_character_delay(-1.0, 3), Duration::ZERO);
        assert_eq!(per_character_delay(f64::NAN, 3), Duration::ZERO);
    }
}
