//! Incursion tracker: polls ESI `/incursions` and records spawns, state changes, the boss,
//! influence samples and ends (`wl_incursions::sync`), then publishes `incursions_update` on the
//! `incursions` topic and posts optional Discord notices.
//!
//! It polls right after ESI's cache expires (the `Expires` header, every 5 minutes), so each
//! ESI refresh is seen once; without that header it waits `incursion_tracker.interval_seconds`.
//! It also maintains the legacy high-sec focus (`incursion_focus`) the same way the legacy
//! incursion updater did, which it replaces while enabled.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use wl_db::Db;
use wl_esi::Esi;
use wl_eve_data::{EveData, Universe};
use wl_incursions::discord::Notifier;
use wl_incursions::sync::{self, SyncError, SyncOutcome};
use wl_live::{Hub, events, topics};

use crate::incursion_updater::{Focus, record_focus};
use crate::runner::{Backoff, sleep};
use crate::{Error, Result};

/// Poll at least this long apart, whatever `Expires` says.
const MIN_DELAY: Duration = Duration::from_secs(30);
/// Wait this long past `Expires`: ESI's cache and clocks are not exact.
const EXPIRES_SLACK: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct IncursionTracker {
    pub db: Db,
    pub esi: Esi,
    pub live: Hub,
    /// The star map for names, roles and security (ESI is asked when it is missing).
    pub eve_data: Option<Arc<EveData>>,
    /// Delay between polls when ESI sends no `Expires` header.
    pub interval: Duration,
    /// Maintain `incursion_focus` (when `incursion_updater.enable`).
    pub maintain_focus: bool,
    pub notifier: Option<Notifier>,
    /// The previous poll was empty while spawns were active (see [`sync::apply`]).
    saw_empty: Arc<AtomicBool>,
}

impl std::fmt::Debug for IncursionTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncursionTracker")
            .field("interval", &self.interval)
            .field("maintain_focus", &self.maintain_focus)
            .finish_non_exhaustive()
    }
}

/// The `incursions_update` payload.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UpdateEvent {
    pub spawn_ids: Vec<i64>,
    pub changes: Vec<ChangeEvent>,
    pub influence_changed: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChangeEvent {
    pub spawn_id: i64,
    pub kind: &'static str,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
}

impl From<&SyncOutcome> for UpdateEvent {
    fn from(outcome: &SyncOutcome) -> Self {
        Self {
            spawn_ids: outcome.spawn_ids(),
            changes: outcome
                .changes
                .iter()
                .map(|c| ChangeEvent {
                    spawn_id: c.spawn.id,
                    kind: c.kind,
                    from_state: c.from_state.clone(),
                    to_state: c.to_state.clone(),
                })
                .collect(),
            influence_changed: outcome.influence_changed.clone(),
        }
    }
}

impl From<SyncError> for Error {
    fn from(err: SyncError) -> Self {
        match err {
            SyncError::Database(e) => Self::Database(e),
            SyncError::Esi(e) => Self::Esi(e),
        }
    }
}

impl IncursionTracker {
    pub fn new(
        db: Db,
        esi: Esi,
        live: Hub,
        eve_data: Option<Arc<EveData>>,
        interval: Duration,
    ) -> Self {
        Self {
            db,
            esi,
            live,
            eve_data,
            interval,
            maintain_focus: false,
            notifier: None,
            saw_empty: Arc::default(),
        }
    }

    pub fn with_focus(mut self, maintain_focus: bool) -> Self {
        self.maintain_focus = maintain_focus;
        self
    }

    pub fn with_notifier(mut self, notifier: Option<Notifier>) -> Self {
        self.notifier = notifier;
        self
    }

    pub async fn run(self, cancel: CancellationToken) {
        tracing::info!(
            worker = "incursion_tracker",
            fallback_interval_secs = self.interval.as_secs(),
            "worker started"
        );
        let mut backoff = Backoff::new(Duration::from_secs(60), Duration::from_secs(1800));
        loop {
            let result = tokio::select! {
                () = cancel.cancelled() => break,
                result = self.run_once() => result,
            };
            let delay = match result {
                Ok(delay) => {
                    backoff.success();
                    delay
                }
                Err(e) => {
                    let delay = backoff.failure();
                    tracing::error!(
                        worker = "incursion_tracker",
                        error = %e,
                        retry_in_secs = delay.as_secs(),
                        "worker cycle failed"
                    );
                    delay
                }
            };
            if !sleep(&cancel, delay).await {
                break;
            }
        }
        tracing::info!(worker = "incursion_tracker", "worker stopped");
    }

    /// One poll. Returns how long to wait before the next.
    pub async fn run_once(&self) -> Result<Duration> {
        let (outcome, delay) = self.poll().await?;
        if outcome.changed() {
            let event = UpdateEvent::from(&outcome);
            if let Err(e) =
                self.live
                    .publish_json(topics::INCURSIONS, events::INCURSIONS_UPDATE, &event)
            {
                tracing::error!(error = %e, "could not encode the incursions update");
            }
        }
        for change in &outcome.changes {
            tracing::info!(
                spawn_id = change.spawn.id,
                constellation = %change.spawn.constellation_name,
                kind = change.kind,
                to = change.to_state.as_deref().unwrap_or(""),
                "incursion change"
            );
        }
        if let Some(notifier) = &self.notifier {
            notifier.notify(&outcome.changes).await;
        }
        Ok(delay)
    }

    /// Fetch, apply and maintain the focus; the outcome and the delay until the next poll.
    pub async fn poll(&self) -> Result<(SyncOutcome, Duration)> {
        let snapshot = self.esi.incursions_snapshot().await?;
        let empty_universe = Universe::default();
        let universe = self
            .eve_data
            .as_deref()
            .map_or(&empty_universe, EveData::universe);
        let now = chrono::Utc::now();
        let trust_empty = self.saw_empty.load(Ordering::Relaxed);
        let outcome = sync::apply(
            &self.db,
            &self.esi,
            universe,
            &snapshot,
            now.timestamp(),
            trust_empty,
        )
        .await?;
        self.saw_empty
            .store(outcome.ignored_empty, Ordering::Relaxed);
        if outcome.ignored_empty {
            tracing::warn!("ESI listed no incursions; ending the active ones if it repeats");
        } else if self.maintain_focus {
            let focus = outcome
                .focus
                .clone()
                .map(|(constellation_id, constellation_name)| Focus {
                    constellation_id,
                    constellation_name,
                });
            record_focus(&self.db, focus).await?;
        }

        let delay = snapshot
            .expires
            .and_then(|expires| (expires - now).to_std().ok())
            .map_or(self.interval, |until| until + EXPIRES_SLACK)
            .clamp(MIN_DELAY, self.interval.max(MIN_DELAY) * 2);
        Ok((outcome, delay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_event_serializes() {
        let event = UpdateEvent {
            spawn_ids: vec![1],
            changes: vec![ChangeEvent {
                spawn_id: 1,
                kind: "state_changed",
                from_state: Some("established".into()),
                to_state: Some("mobilizing".into()),
            }],
            influence_changed: vec![],
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "spawn_ids": [1],
                "changes": [{"spawn_id": 1, "kind": "state_changed",
                    "from_state": "established", "to_state": "mobilizing"}],
                "influence_changed": [],
            })
        );
    }
}
