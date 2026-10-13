//! EVE SSO and ESI for the waitlist: login URLs, code exchange, database-backed tokens and
//! typed wrappers for every ESI call the waitlist makes.
//!
//! Everything goes through one [`Esi`] handle (cheap to clone). It owns a single
//! [`eve_esi_client::Client`], so every character's requests share ESI's error budget,
//! rate-limit groups and response cache; per-character requests are made with that character's
//! access token, loaded from (and refreshed into) the `access_token`/`refresh_token` tables.
//!
//! ```ignore
//! let esi = wl_esi::Esi::new(&config, db)?;
//! let url = esi.login_url(wl_esi::LoginKind::Normal, false);
//! let auth = esi.process_authorization_code(&code).await?;   // stores the tokens
//! let fleet = esi.current_fleet(auth.character_id).await?;  // as that character
//! ```
//!
//! Failures are [`EsiError`] (shared with the API's error mapping; see [`error`]).

pub mod error;
mod esi;
mod scope;
mod sso;
mod tokens;
mod util;

pub mod character;
pub mod fleet;
pub mod killmail;
pub mod universe;
pub mod wallet;
pub mod war;

pub use error::{EsiError, EsiErrorExt, EsiResult};
pub use esi::{Esi, EsiOptions, EsiResponse, SetupError, default_user_agent};
pub use scope::{LoginKind, Scope, join_scopes, split_scopes};
pub use sso::AuthResult;

/// The underlying client crate, for callers that need an endpoint without a wrapper here.
pub use eve_esi_client;
