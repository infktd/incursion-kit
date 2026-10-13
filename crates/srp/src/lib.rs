//! Ship replacement program (SRP): loss reports, subscription coverage, wallet reconciliation,
//! Janice appraisals, Discord notices and the corporation-wallet service account.
//!
//! # How SRP works
//!
//! Pilots buy coverage by sending ISK to a corporation wallet. A "service account" character
//! with the wallet-read role (set up through SSO, see [`service_account`]) lets the backend read
//! that wallet's journal. Every reconciliation run ([`Srp::process_payments`], run by the
//! [`updater`] worker and the admin "process" button):
//!
//! 1. reads the journal and **rebuilds the `srp_payments` table** from the deposits of the last
//!    `srp.journal_days` (8) days: the table is wiped and refilled each run, so it never holds
//!    older payments. A deposit buys coverage when its amount matches a configured price (see
//!    [`tiers`]); coverage is keyed by the depositor's **character name** as written in the
//!    journal description, not by character id;
//! 2. matches outgoing journal entries (payouts made by hand in game) by recipient name and exact
//!    amount against `approved` reports and marks them `paid`;
//! 3. voids per-focus coverage once the incursion focus has ended ([`focus`]).
//!
//! Approving a report moves no ISK: payment happens in game and is detected afterwards.
//!
//! The tier prices and coverage rules are configuration (`[srp]` in `config.toml`, defaults
//! equal to the legacy waitlist's), see [`wl_core::config::SrpConfig`].

pub mod coverage;
pub mod discord;
pub mod focus;
pub mod janice;
pub mod killmail;
pub mod payments;
pub mod reports;
pub mod service_account;
pub mod tiers;
pub mod types;
pub mod updater;

mod error;
mod time;

use std::sync::Arc;

use wl_core::Config;
use wl_core::config::{DiscordConfig, SrpConfig};
use wl_db::Db;
use wl_esi::Esi;
use wl_eve_data::EveData;

pub use error::{SrpError, SrpResult};
pub use types::*;

/// Default Janice API origin.
pub const JANICE_URL: &str = "https://janice.e-351.com";

/// The SRP service: database, ESI, configuration and outgoing HTTP (Janice, Discord). Cheap to
/// clone.
#[derive(Clone)]
pub struct Srp {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    esi: Esi,
    eve: Option<Arc<EveData>>,
    http: reqwest::Client,
    settings: Settings,
}

/// The configuration SRP uses.
#[derive(Debug, Clone)]
pub struct Settings {
    pub tiers: SrpConfig,
    /// `srp_updater.fc_srp_price_factor`.
    pub fc_price_factor: f64,
    /// `srp_updater.interval_seconds`.
    pub interval_seconds: u64,
    pub janice_api_key: String,
    /// Janice origin (no trailing slash). [`JANICE_URL`] unless overridden (tests).
    pub janice_url: String,
    pub discord: DiscordConfig,
    /// Origin of the public site, for links in Discord notices.
    pub site_url: String,
}

impl Settings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            tiers: config.srp.clone(),
            fc_price_factor: config.srp_updater.fc_srp_price_factor,
            interval_seconds: config.srp_updater.interval_seconds,
            janice_api_key: config.janice.api_key.clone(),
            janice_url: JANICE_URL.to_string(),
            discord: config.discord.clone(),
            site_url: discord::site_url(&config.discord.site_url, &config.esi.url),
        }
    }
}

impl std::fmt::Debug for Srp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Srp").finish_non_exhaustive()
    }
}

impl Srp {
    pub fn new(config: &Config, db: Db, esi: Esi) -> Self {
        Self::with_settings(Settings::from_config(config), db, esi)
    }

    pub fn with_settings(settings: Settings, db: Db, esi: Esi) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(wl_esi::default_user_agent(&settings.site_url))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                db,
                esi,
                eve: None,
                http,
                settings,
            }),
        }
    }

    /// Use the static data for type names (ship and item names) before asking ESI.
    pub fn with_eve_data(self, eve: Arc<EveData>) -> Self {
        let inner = &self.inner;
        Self {
            inner: Arc::new(Inner {
                db: inner.db.clone(),
                esi: inner.esi.clone(),
                eve: Some(eve),
                http: inner.http.clone(),
                settings: inner.settings.clone(),
            }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn esi(&self) -> &Esi {
        &self.inner.esi
    }

    pub fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    /// The FC price factor as the legacy waitlist applied it: values outside (0, 1] mean 0.5.
    pub fn fc_price_factor(&self) -> f64 {
        let factor = self.inner.settings.fc_price_factor;
        if factor > 0.0 && factor <= 1.0 {
            factor
        } else {
            0.5
        }
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn eve(&self) -> Option<&EveData> {
        self.inner.eve.as_deref()
    }

    /// A type's name: static data first, then ESI (bounded by `timeout`). `None` if both fail.
    pub(crate) async fn type_name(&self, type_id: i64, timeout_secs: u64) -> Option<String> {
        if let Some(name) = i32::try_from(type_id)
            .ok()
            .and_then(|id| self.eve().and_then(|eve| eve.name_of(id)))
        {
            return Some(name.to_string());
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            self.esi().type_name(type_id),
        )
        .await
        .ok()
        .and_then(Result::ok)
    }
}
