//! Incursion updater: tracks the high-sec incursion focus in `incursion_focus` (read by the SRP
//! pages and the per-focus SRP coverage).
//!
//! Each run finds the first active incursion whose staging system is high-sec (security status
//! ≥ 0.5), records it as the current focus, and stamps `focus_end_timestamp` when an active focus
//! disappears. Before writing, it voids per-focus SRP coverage bought before the last focus end
//! (`wl_srp::focus::check_srp_validity`), as the legacy updater did.
//!
//! When the incursion tracker runs (`incursion_tracker.enable`), it maintains the focus through
//! [`record_focus`] instead and this updater is not started.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wl_db::Db;
use wl_esi::Esi;

use crate::Result;
use crate::runner::{self, Backoff};

/// Lowest security status of a high-sec system.
pub const HIGHSEC: f64 = 0.5;

#[derive(Clone, Debug)]
pub struct IncursionUpdater {
    pub db: Db,
    pub esi: Esi,
    /// Seconds between runs (`incursion_updater.interval_seconds`).
    pub interval: Duration,
    /// Constellation names already looked up.
    constellation_names: Arc<Mutex<HashMap<i64, String>>>,
}

/// A detected high-sec focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Focus {
    pub constellation_id: i64,
    pub constellation_name: String,
}

/// Stores the detected focus in `incursion_focus` (the legacy updater's write, shared with the
/// incursion tracker): updates the latest row (or inserts the first), stamps
/// `focus_end_timestamp` when an active focus disappears, and first voids per-focus SRP coverage
/// bought before the last focus end.
pub async fn record_focus(db: &Db, detected: Option<Focus>) -> Result<()> {
    let current = wl_srp::focus::current_focus(db).await?;
    let now = chrono::Utc::now().timestamp();

    let was_active = current.as_ref().is_some_and(|f| f.focus_active);
    let focus_end = if was_active && detected.is_none() {
        Some(now)
    } else {
        current.as_ref().and_then(|f| f.focus_end_timestamp)
    };

    wl_srp::focus::check_srp_validity(db).await?;

    let constellation_id = detected.as_ref().map(|f| f.constellation_id);
    let constellation_name = detected.as_ref().map(|f| f.constellation_name.as_str());
    match &current {
        Some(row) => {
            sqlx::query(
                "UPDATE incursion_focus SET current_focus_constellation_id = ?, \
                 current_focus_constellation_name = ?, last_check_timestamp = ?, \
                 focus_active = ?, focus_end_timestamp = ?, updated_at = ? WHERE id = ?",
            )
            .bind(constellation_id)
            .bind(constellation_name)
            .bind(now)
            .bind(detected.is_some())
            .bind(focus_end)
            .bind(now)
            .bind(row.id)
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO incursion_focus (current_focus_constellation_id, \
                 current_focus_constellation_name, last_check_timestamp, focus_active, \
                 focus_end_timestamp, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(constellation_id)
            .bind(constellation_name)
            .bind(now)
            .bind(detected.is_some())
            .bind(focus_end)
            .bind(now)
            .bind(now)
            .execute(db)
            .await?;
        }
    }

    let previous = current.and_then(|f| f.current_focus_constellation_id);
    if previous != constellation_id {
        match &detected {
            Some(focus) => tracing::info!(
                constellation_id = focus.constellation_id,
                constellation = %focus.constellation_name,
                "new high-sec incursion focus"
            ),
            None => tracing::info!("the high-sec incursion focus has ended"),
        }
    }
    Ok(())
}

impl IncursionUpdater {
    pub fn new(db: Db, esi: Esi, interval: Duration) -> Self {
        Self {
            db,
            esi,
            interval,
            constellation_names: Default::default(),
        }
    }

    pub async fn run(self, cancel: CancellationToken) {
        runner::run(
            "incursion_updater",
            cancel,
            self.interval,
            Backoff::new(Duration::from_secs(300), Duration::from_secs(3600)),
            || self.run_once(),
        )
        .await;
    }

    /// Detect the focus and store it.
    pub async fn run_once(&self) -> Result<()> {
        let detected = self.detect_highsec_focus().await?;
        record_focus(&self.db, detected).await
    }

    /// The first incursion staged in high-sec, if any.
    pub async fn detect_highsec_focus(&self) -> Result<Option<Focus>> {
        for incursion in self.esi.incursions().await? {
            let system = self
                .esi
                .solar_system(incursion.staging_solar_system_id)
                .await?;
            if system.security_status >= HIGHSEC {
                let constellation_name =
                    self.constellation_name(incursion.constellation_id).await?;
                return Ok(Some(Focus {
                    constellation_id: incursion.constellation_id,
                    constellation_name,
                }));
            }
        }
        Ok(None)
    }

    async fn constellation_name(&self, constellation_id: i64) -> Result<String> {
        let cached = self
            .constellation_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&constellation_id)
            .cloned();
        if let Some(name) = cached {
            return Ok(name);
        }
        let name = self.esi.constellation(constellation_id).await?.name;
        self.constellation_names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(constellation_id, name.clone());
        Ok(name)
    }
}
