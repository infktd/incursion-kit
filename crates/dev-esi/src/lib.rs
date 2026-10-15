//! dev-esi: a local stand-in for EVE Online's SSO and ESI, so the waitlist (and a frontend on
//! top of it) runs end to end without a CCP account. **Development and tests only**: it hands
//! out tokens for any seeded character to anyone who asks.
//!
//! - **SSO** (`/v2/oauth/authorize`, `/v2/oauth/token`, `/oauth/jwks`): a login page listing the
//!   seeded characters, authorization codes (client secret or PKCE), refresh tokens that rotate,
//!   and RS256 JWT access tokens with EVE's claims, granting exactly the requested scopes.
//! - **ESI**: every endpoint the waitlist calls, with ESI's paths, JSON shapes, status codes,
//!   error bodies (`{"error": "..."}`), scope checks and `ETag`/`Expires` headers. Fleets are
//!   simulated: invited pilots accept after a delay and join the requested squad.
//! - **Control API** (`/_dev/*`, JSON) and a dashboard (`/_dev`) to change the world: characters,
//!   skills, implants, fleets, invites, killmails, wallet journal, wars, incursions, reset.
//!
//! The world starts from a [`seed`] ([`seed::DEFAULT_SEED`] unless one is given). Use it from
//! tests with [`DevEsi::spawn`]:
//!
//! ```ignore
//! let dev = dev_esi::DevEsi::new(SeedSource::default_seed(), Signer::generate()?, None)?;
//! let running = dev.spawn("127.0.0.1:0").await?;
//! // point esi.esi_url at running.url, esi.sso_token_url at running.url + "/v2/oauth/token", ...
//! ```

pub mod control;
pub mod esi;
pub mod jwt;
pub mod seed;
pub mod sso;
pub mod universe;
pub mod world;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use axum::Router;
use tokio::net::TcpListener;

pub use jwt::Signer;
pub use seed::{Seed, SeedError, SeedSource};
pub use universe::Universe;
pub use world::{DevError, Role, World};

/// A dev-esi instance. Cheap to clone; clones share the world.
#[derive(Clone)]
pub struct DevEsi {
    inner: Arc<Inner>,
}

struct Inner {
    world: Mutex<World>,
    signer: Signer,
    universe: Universe,
    /// Where refresh tokens are kept, if anywhere ([`DevEsi::persist_grants`]).
    grants_file: OnceLock<PathBuf>,
}

impl std::fmt::Debug for DevEsi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevEsi").finish_non_exhaustive()
    }
}

impl DevEsi {
    /// A world from `seed`, signing tokens with `signer`, serving static data from `universe`
    /// (without it, the universe endpoints know only the seed's `names`).
    pub fn new(
        seed: SeedSource,
        signer: Signer,
        universe: Option<Universe>,
    ) -> Result<Self, SeedError> {
        Ok(Self {
            inner: Arc::new(Inner {
                world: Mutex::new(World::new(seed)?),
                signer,
                universe: universe.unwrap_or_default(),
                grants_file: OnceLock::new(),
            }),
        })
    }

    /// Keep refresh tokens in `path` (JSON), starting with the ones already there, so the tokens
    /// a waitlist stored survive a dev-esi restart (with the signing key kept as well).
    pub fn persist_grants(&self, path: PathBuf) -> std::io::Result<()> {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let grants: HashMap<String, world::RefreshGrant> =
                    serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
                self.world().sso.refresh_tokens.extend(grants);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let _ = self.inner.grants_file.set(path);
        Ok(())
    }

    /// Write the refresh tokens out (when [`DevEsi::persist_grants`] was called).
    pub(crate) fn save_grants(&self, world: &World) {
        if let Some(path) = self.inner.grants_file.get()
            && let Err(e) = serde_json::to_vec(&world.sso.refresh_tokens)
                .map_err(std::io::Error::other)
                .and_then(|bytes| std::fs::write(path, bytes))
        {
            tracing::warn!(path = %path.display(), error = %e, "could not save refresh tokens");
        }
    }

    /// Lock the world (briefly: request handlers wait on it).
    pub fn world(&self) -> MutexGuard<'_, World> {
        self.inner
            .world
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn signer(&self) -> &Signer {
        &self.inner.signer
    }

    pub fn universe(&self) -> &Universe {
        &self.inner.universe
    }

    /// Every route: SSO, ESI and the control API.
    pub fn router(&self) -> Router {
        Router::new()
            .merge(sso::routes())
            .merge(esi::routes())
            .merge(control::routes())
            .with_state(self.clone())
    }

    /// Serve on `listener` until the task is dropped or aborted.
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }

    /// Serve on `addr` (e.g. `127.0.0.1:0`) in a background task.
    pub async fn spawn(self, addr: &str) -> std::io::Result<Running> {
        let listener = TcpListener::bind(addr).await?;
        let url = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(self.clone().serve(listener));
        Ok(Running {
            url,
            dev: self,
            task,
        })
    }

    /// Accept `character_id`'s invite `invite_id` after the configured delay (if it is still
    /// pending then).
    pub(crate) fn schedule_accept(&self, character_id: i64, invite_id: i64) {
        let delay = self.world().settings.invite_accept_delay_ms;
        let dev = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            if let Err(e) = dev.world().accept_invite(character_id, Some(invite_id)) {
                tracing::debug!(character_id, invite_id, error = %e, "automatic accept skipped");
            }
        });
    }
}

/// A dev-esi serving in the background (stops when dropped).
#[derive(Debug)]
pub struct Running {
    /// `http://127.0.0.1:<port>`: the ESI base URL; SSO is under `/v2/oauth/*`.
    pub url: String,
    pub dev: DevEsi,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    pub fn authorize_url(&self) -> String {
        format!("{}/v2/oauth/authorize", self.url)
    }

    pub fn token_url(&self) -> String {
        format!("{}/v2/oauth/token", self.url)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// HTML-escape text for the pages.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
