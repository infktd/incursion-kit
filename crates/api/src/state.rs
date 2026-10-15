//! Shared application state handed to every handler (`State<AppState>`).
//!
//! `AppState` is cheap to clone (everything is behind `Arc`s or is itself a handle).
//!
//! # Adding a service
//!
//! Services owned by other crates (ESI client, EVE static data, fitting store, ...) are added as
//! `Option<T>` fields with a `with_*` builder method and an accessor returning
//! `Result<&T, ApiError>` (via [`require`]):
//!
//! ```ignore
//! // field:     esi: Option<wl_esi::Esi>,
//! // builder:   pub fn with_esi(mut self, esi: wl_esi::Esi) -> Self { self.esi = Some(esi); self }
//! // accessor:  pub fn esi(&self) -> Result<&wl_esi::Esi, ApiError> { require(&self.esi, "ESI client") }
//! ```
//!
//! The binary wires every service; tests only wire what they exercise, and a handler that needs a
//! missing service fails with a logged 500 instead of the whole state being unconstructible.

use std::sync::Arc;

use wl_content::Content;
use wl_core::Config;
use wl_core::config::ConfigError;
use wl_db::Db;
use wl_esi::Esi;
use wl_eve_data::EveData;
use wl_fitting::DataStore;
use wl_live::Hub;
use wl_srp::Srp;
use wl_workers::affiliation::Affiliation;
use wl_workers::war::WarCache;

use crate::ApiError;
use crate::auth::SessionKey;

#[derive(Clone)]
pub struct AppState {
    db: Db,
    config: Arc<Config>,
    live: Hub,
    session_key: SessionKey,
    content: Content,
    esi: Option<Esi>,
    eve_data: Option<Arc<EveData>>,
    data_store: Option<Arc<DataStore>>,
    srp: Option<Srp>,
    war_cache: WarCache,
    wiki_search: wl_wiki::SearchCache,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// State with the core services. Fails only if `app.token_secret` is invalid.
    pub fn new(config: Config, db: Db, live: Hub) -> Result<Self, ConfigError> {
        let session_key = SessionKey::new(config.app.token_key()?);
        let content = Content::new(&config.data.dir);
        Ok(Self {
            db,
            config: Arc::new(config),
            live,
            session_key,
            content,
            esi: None,
            eve_data: None,
            data_store: None,
            srp: None,
            war_cache: WarCache::new(),
            wiki_search: wl_wiki::SearchCache::default(),
        })
    }

    /// Use another content directory than `config.data.dir` (tests).
    pub fn with_content(mut self, content: Content) -> Self {
        self.content = content;
        self
    }

    pub fn with_esi(mut self, esi: Esi) -> Self {
        self.esi = Some(esi);
        self
    }

    pub fn with_eve_data(mut self, eve_data: Arc<EveData>) -> Self {
        self.eve_data = Some(eve_data);
        self
    }

    /// The community data files (fits, categories, skills, ...). Every change of the data set
    /// (an edit through the API or a reload) is announced as `data_update` on the live `data`
    /// topic.
    pub fn with_data_store(mut self, data_store: Arc<DataStore>) -> Self {
        let live = self.live.clone();
        data_store.on_change(move |file| live.data_changed(file.map(|file| file.file_name())));
        self.data_store = Some(data_store);
        self
    }

    /// The SRP service (reports, coverage, wallet reconciliation, Discord notices).
    pub fn with_srp(mut self, srp: Srp) -> Self {
        self.srp = Some(srp);
        self
    }

    /// The war cache the war updater fills (an empty one by default: nobody is at war).
    pub fn with_war_cache(mut self, war_cache: WarCache) -> Self {
        self.war_cache = war_cache;
        self
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The live-event hub: `state.live().publish_json(topic, event, &payload)`.
    pub fn live(&self) -> &Hub {
        &self.live
    }

    pub fn session_key(&self) -> &SessionKey {
        &self.session_key
    }

    /// File-backed site content (locales, guides, guide images) under `config.data.dir`. Always
    /// available: it only needs the directory.
    pub fn content(&self) -> &Content {
        &self.content
    }

    pub fn esi(&self) -> Result<&Esi, ApiError> {
        require(&self.esi, "ESI client")
    }

    /// EVE static data (type names, ...).
    pub fn eve_data(&self) -> Result<&Arc<EveData>, ApiError> {
        require(&self.eve_data, "EVE static data")
    }

    pub fn data_store(&self) -> Result<&Arc<DataStore>, ApiError> {
        require(&self.data_store, "community data store")
    }

    pub fn srp(&self) -> Result<&Srp, ApiError> {
        require(&self.srp, "SRP service")
    }

    /// Corporations and alliances at war (`war_cache().character_is_at_war(..)` for x-up,
    /// `character_ids_at_war(..)` for the fleet view).
    pub fn war_cache(&self) -> &WarCache {
        &self.war_cache
    }

    /// Affiliation refresh (character/corporation/alliance from ESI). Needs the ESI client.
    pub fn affiliation(&self) -> Result<Affiliation, ApiError> {
        Ok(Affiliation::new(self.esi()?.clone(), self.db.clone()))
    }

    /// The wiki (`wl-wiki`): pages in the database, images under the content directory's
    /// `wiki/assets`. Every change is announced as `wiki_update`: on the `wiki` topic when
    /// anonymous visitors could see the page (before or after), else on `wiki_restricted`.
    pub fn wiki(&self) -> wl_wiki::Wiki {
        let live = self.live.clone();
        wl_wiki::Wiki::new(
            self.db.clone(),
            self.content.dir(),
            self.wiki_search.clone(),
        )
        .with_notifier(move |change| {
            let topic = if change.public {
                wl_live::topics::WIKI
            } else {
                wl_live::topics::WIKI_RESTRICTED
            };
            if let Err(error) = live.publish_json(topic, wl_live::events::WIKI_UPDATE, change) {
                tracing::error!(%error, "could not encode a wiki event");
            }
        })
    }

    /// Whether cookies get the `Secure` attribute (the public site is HTTPS).
    pub fn secure_cookies(&self) -> bool {
        self.config.is_https()
    }
}

/// Accessor helper for optional services: a missing service is a server bug, not a client error.
pub(crate) fn require<'a, T>(service: &'a Option<T>, name: &str) -> Result<&'a T, ApiError> {
    service
        .as_ref()
        .ok_or_else(|| ApiError::internal(format!("{name} is not configured")))
}
