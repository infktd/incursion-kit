//! Background workers and the services they share with the API.
//!
//! Workers (each a loop that stops when its [`CancellationToken`] is cancelled, logs failures
//! and backs off after them):
//! - [`fleet_updater`]: polls registered fleets, removes pilots who joined from the waitlists,
//!   tracks `fleet_activity`, publishes `waitlist_update`/`comp_update`.
//! - [`skill_updater`]: refreshes every pilot's skills.
//! - [`war_updater`]: tracks active wars and fills the [`war::WarCache`].
//! - [`incursion_tracker`]: the public incursion tracker (spawns, influence, state changes);
//!   also maintains the high-sec focus while it runs.
//! - [`incursion_updater`]: tracks the high-sec incursion focus (only when the tracker is
//!   disabled).
//!
//! The SRP updater lives in `wl-srp`.
//!
//! Shared services, also used by the API:
//! - [`affiliation::Affiliation`]: character/corporation/alliance refresh from ESI.
//! - [`war::WarCache`]: corporations and alliances at war (x-up, fleet composition).
//! - [`skills::load_skills`]: a pilot's skills from ESI into `skill_current`/`skill_history`.
//!
//! [`start`] spawns the workers the configuration enables.

pub mod affiliation;
mod error;
pub mod fleet_updater;
pub mod incursion_tracker;
pub mod incursion_updater;
mod runner;
pub mod skill_updater;
pub mod skills;
pub mod war;
pub mod war_updater;

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
pub use tokio_util::sync::CancellationToken;
use wl_core::Config;
use wl_db::Db;
use wl_esi::Esi;
use wl_eve_data::EveData;
use wl_live::Hub;

pub use error::{Error, Result};

/// Everything the workers need.
#[derive(Clone, Debug)]
pub struct Services {
    pub config: Config,
    pub db: Db,
    pub esi: Esi,
    pub live: Hub,
    pub war_cache: war::WarCache,
    /// Static data with the star map (incursion tracker enrichment); optional.
    pub eve_data: Option<Arc<EveData>>,
}

/// Spawn every worker enabled in the configuration. They run until `cancel` is cancelled; await
/// the handles to wait for them to stop.
pub fn start(services: Services, cancel: &CancellationToken) -> Vec<JoinHandle<()>> {
    let Services {
        config,
        db,
        esi,
        live,
        war_cache,
        eve_data,
    } = services;
    let mut handles = Vec::new();

    if config.fleet_updater.enable {
        let worker = fleet_updater::FleetUpdater {
            db: db.clone(),
            esi: esi.clone(),
            live: live.clone(),
            min_in_fleet: config.fleet_updater.min_in_fleet,
        };
        handles.push(tokio::spawn(worker.run(cancel.child_token())));
    }
    if config.skill_updater.enable {
        let worker = skill_updater::SkillUpdater {
            db: db.clone(),
            esi: esi.clone(),
            runtime: config.skill_updater.runtime,
        };
        handles.push(tokio::spawn(worker.run(cancel.child_token())));
    }
    if config.war_updater.enable {
        let worker = war_updater::WarUpdater {
            db: db.clone(),
            esi: esi.clone(),
            cache: war_cache,
            interval: interval(config.war_updater.interval_seconds),
        };
        handles.push(tokio::spawn(worker.run(cancel.child_token())));
    }
    let tracker = &config.incursion_tracker;
    if tracker.enable {
        let worker = incursion_tracker::IncursionTracker::new(
            db.clone(),
            esi.clone(),
            live,
            eve_data,
            interval(tracker.interval_seconds),
        )
        .with_focus(config.incursion_updater.enable)
        .with_notifier(wl_incursions::discord::Notifier::new(&tracker.discord));
        handles.push(tokio::spawn(worker.run(cancel.child_token())));
    } else if config.incursion_updater.enable {
        let worker = incursion_updater::IncursionUpdater::new(
            db,
            esi,
            interval(config.incursion_updater.interval_seconds),
        );
        handles.push(tokio::spawn(worker.run(cancel.child_token())));
    }
    handles
}

/// A configured interval in seconds (at least one second).
fn interval(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}
