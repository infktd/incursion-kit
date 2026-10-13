//! The [`Esi`] handle and its construction.

use std::sync::Arc;

use eve_esi_client::auth::SsoClient;
use eve_esi_client::{Client, ResponseValue};
use wl_core::config::EsiConfig;
use wl_db::Db;

use crate::error::{self, ErrorStyle, EsiResult};
use crate::scope::Scope;
use crate::tokens::TokenStore;

/// Everything needed to talk to ESI and EVE SSO. [`EsiOptions::from_config`] fills it from the
/// `[esi]` config section; tests point the URLs at a mock server.
#[derive(Clone)]
pub struct EsiOptions {
    pub client_id: String,
    pub client_secret: String,
    /// The SSO callback URL registered with CCP (`esi.url`).
    pub redirect_url: String,
    pub user_agent: String,
    /// ESI base URL (default: [`eve_esi_client::BASE_URL`]).
    pub esi_url: String,
    /// EVE SSO authorize endpoint (default: [`eve_esi_client::SSO_AUTHORIZE_URL`]).
    pub sso_authorize_url: String,
    /// EVE SSO token endpoint (default: [`eve_esi_client::SSO_TOKEN_URL`]).
    pub sso_token_url: String,
}

impl EsiOptions {
    pub fn from_config(config: &EsiConfig) -> Self {
        Self {
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            redirect_url: config.url.clone(),
            user_agent: config
                .user_agent
                .clone()
                .filter(|ua| !ua.trim().is_empty())
                .unwrap_or_else(|| default_user_agent(&config.url)),
            esi_url: non_empty(&config.esi_url)
                .unwrap_or(eve_esi_client::BASE_URL)
                .to_string(),
            sso_authorize_url: non_empty(&config.sso_authorize_url)
                .unwrap_or(eve_esi_client::SSO_AUTHORIZE_URL)
                .to_string(),
            sso_token_url: non_empty(&config.sso_token_url)
                .unwrap_or(eve_esi_client::SSO_TOKEN_URL)
                .to_string(),
        }
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl std::fmt::Debug for EsiOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EsiOptions")
            .field("client_id", &self.client_id)
            .field("redirect_url", &self.redirect_url)
            .field("user_agent", &self.user_agent)
            .field("esi_url", &self.esi_url)
            .finish_non_exhaustive()
    }
}

/// The `User-Agent` used when `esi.user_agent` isn't configured: the app name and version plus
/// the site's origin (from the SSO callback URL), which is how CCP can reach the operator.
pub fn default_user_agent(site_url: &str) -> String {
    let origin = site_url
        .split_once("://")
        .map(|(scheme, rest)| format!("{scheme}://{}", rest.split('/').next().unwrap_or(rest)))
        .unwrap_or_else(|| site_url.to_string());
    format!("incursion-kit/{} (+{origin})", env!("CARGO_PKG_VERSION"))
}

/// Building an [`Esi`] failed (an invalid URL or `User-Agent`, or the HTTP client).
#[derive(Debug, thiserror::Error)]
#[error("could not set up the ESI client: {0}")]
pub struct SetupError(String);

/// A response with the `ETag` it came with (without quotes), for callers that store ETags.
#[derive(Debug, Clone)]
pub struct EsiResponse<T> {
    pub data: T,
    pub etag: Option<String>,
}

/// Handle to ESI and EVE SSO. Cheap to clone; clones share connections, limits, cache and token
/// refresh locks.
#[derive(Clone)]
pub struct Esi {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    /// Unauthenticated; per-character clients are derived from it.
    pub(crate) client: Client,
    pub(crate) sso: Arc<SsoClient>,
    pub(crate) tokens: TokenStore,
}

impl std::fmt::Debug for Esi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Esi").finish_non_exhaustive()
    }
}

impl Esi {
    /// The client for the configured application, storing tokens in `db`.
    pub fn new(config: &wl_core::Config, db: Db) -> Result<Self, SetupError> {
        Self::with_options(EsiOptions::from_config(&config.esi), db)
    }

    pub fn with_options(options: EsiOptions, db: Db) -> Result<Self, SetupError> {
        let setup = |e: &dyn std::fmt::Display| SetupError(e.to_string());
        let client = Client::builder()
            .user_agent(options.user_agent.clone())
            .base_url(options.esi_url.trim_end_matches('/'))
            .build()
            .map_err(|e| setup(&e))?;
        // Token requests identify the application too, never follow redirects (they would carry
        // the code or refresh token elsewhere) and time out like the default SSO client.
        let sso_http = reqwest::Client::builder()
            .user_agent(options.user_agent.clone())
            .timeout(eve_esi_client::DEFAULT_TIMEOUT)
            .connect_timeout(eve_esi_client::DEFAULT_CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| setup(&e))?;
        let sso = SsoClient::builder(options.client_id.clone(), options.redirect_url.clone())
            .client_secret(options.client_secret.clone())
            .authorize_url(options.sso_authorize_url.clone())
            .token_url(options.sso_token_url.clone())
            .http_client(sso_http)
            .build()
            .map_err(|e| setup(&e))?;
        let sso = Arc::new(sso);
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                tokens: TokenStore::new(db, sso.clone()),
                sso,
            }),
        })
    }

    /// The shared, unauthenticated ESI client, for endpoints without a wrapper here.
    pub fn client(&self) -> &Client {
        &self.inner.client
    }

    /// A client making requests as `character_id`, after checking its stored token has `scope`
    /// (refreshing the token first if it has expired). It shares limits and cache with every
    /// other client from this handle.
    ///
    /// Fails with [`crate::EsiError::NoToken`] when no usable token is stored (none, or EVE
    /// revoked it), [`crate::EsiError::MissingScope`] when the scope wasn't granted.
    pub async fn client_for(&self, character_id: i64, scope: Scope) -> EsiResult<Client> {
        let token = self.access_token(character_id, scope).await?;
        Ok(self.inner.client.with_access_token(token))
    }
}

/// Run a generated request and unwrap its value, mapping failures in `style`.
pub(crate) async fn send<T>(
    request: impl Future<
        Output = Result<ResponseValue<T>, eve_esi_client::Error<eve_esi_client::types::Error>>,
    >,
    style: ErrorStyle,
) -> EsiResult<T> {
    request
        .await
        .map(ResponseValue::into_inner)
        .map_err(|e| error::from_client(e, style))
}

/// Like [`send`], for requests that may answer `304 Not Modified` to an `If-None-Match` (when
/// the client had nothing cached): `data` is `None` then.
pub(crate) async fn send_with_etag<T>(
    request: impl Future<
        Output = Result<ResponseValue<T>, eve_esi_client::Error<eve_esi_client::types::Error>>,
    >,
    style: ErrorStyle,
) -> EsiResult<EsiResponse<Option<T>>> {
    let etag_of = |headers: &reqwest::header::HeaderMap| {
        headers
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim_start_matches("W/").trim_matches('"').to_string())
    };
    match request.await {
        Ok(value) => Ok(EsiResponse {
            etag: etag_of(value.headers()),
            data: Some(value.into_inner()),
        }),
        Err(eve_esi_client::Error::UnexpectedResponse(response))
            if response.status() == reqwest::StatusCode::NOT_MODIFIED =>
        {
            Ok(EsiResponse {
                etag: etag_of(response.headers()),
                data: None,
            })
        }
        Err(e) => Err(error::from_client(e, style)),
    }
}

/// The `If-None-Match` value for a stored (unquoted) ETag, as the legacy client sent it.
pub(crate) fn quote_etag(etag: &str) -> String {
    format!("\"{}\"", etag.trim_matches('"'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_user_agent_names_the_site() {
        let ua = default_user_agent("https://waitlist.example.com/auth/cb");
        assert!(ua.starts_with("incursion-kit/"), "{ua}");
        assert!(ua.ends_with("(+https://waitlist.example.com)"), "{ua}");
        assert!(default_user_agent("weird").ends_with("(+weird)"));
    }

    #[test]
    fn etags_are_quoted_once() {
        assert_eq!(quote_etag("abc"), "\"abc\"");
        assert_eq!(quote_etag("\"abc\""), "\"abc\"");
    }
}
