//! EVE SSO login: authorize URLs and the code exchange.
//!
//! The waitlist is a confidential (web) application: the frontend's `/auth/cb` page posts the
//! code to the API, so there is nowhere to keep a PKCE verifier between the two steps, and the
//! client secret authenticates the exchange instead (as it always has, so refresh tokens issued
//! to the legacy waitlist keep working).

use std::collections::BTreeSet;

use eve_esi_client::auth::{AuthError, CsrfToken};

use crate::error::{self, EsiError, EsiResult};
use crate::esi::Esi;
use crate::scope::{LoginKind, Scope};
use crate::tokens::StoredAuth;

/// The character that completed an SSO login, whose tokens are now stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    pub character_id: i64,
    pub character_name: String,
    /// Every scope the stored token carries (this login's plus, where EVE allowed it, ones
    /// granted at earlier logins).
    pub scopes: BTreeSet<String>,
}

impl Esi {
    /// The EVE SSO URL to send the browser to for a `kind` login, with `state` set to
    /// [`LoginKind::state`] (what the legacy `GET /api/auth/login_url` returned). `fc` adds the
    /// fleet-command scopes.
    pub fn login_url(&self, kind: LoginKind, fc: bool) -> String {
        self.login_url_with_state(&kind.scopes(fc), kind.state())
    }

    /// The EVE SSO URL for `scopes` with a caller-chosen `state`, e.g.
    /// `format!("{}:{nonce}", kind.state())` to bind the callback to the browser session
    /// ([`LoginKind::from_state`] ignores the suffix).
    pub fn login_url_with_state(&self, scopes: &[Scope], state: &str) -> String {
        self.inner
            .sso
            .authorize_without_pkce(
                scopes.iter().map(Scope::as_str),
                CsrfToken::new(state.to_string()),
            )
            .expect("the SSO client is always built with a client secret")
    }

    /// Exchange a callback's authorization code, store the tokens (creating the `character`
    /// row for a new character) and return who logged in.
    ///
    /// Like the legacy waitlist, when tokens were already stored for the character it asks EVE
    /// for the union of the new and previously stored scopes, so FC scopes survive a later
    /// normal login; if EVE refuses, the new login's scopes are kept.
    ///
    /// A code EVE rejects (expired, reused) fails with `EsiError::WithMessage(400, ..)` so the
    /// API answers 400 rather than 500.
    pub async fn process_authorization_code(&self, code: &str) -> EsiResult<AuthResult> {
        let sso = &self.inner.sso;
        let tokens = sso.exchange_without_pkce(code).await.map_err(|e| match e {
            AuthError::Rejected { error, description } => EsiError::WithMessage(
                400,
                description.map_or(error.clone(), |d| format!("{error}: {d}")),
            ),
            other => error::sso(other),
        })?;
        let mut auth = StoredAuth::from_tokens(&tokens).ok_or_else(|| {
            EsiError::Http("EVE SSO returned a token that doesn't identify a character".into())
        })?;

        if let Some(previous) = self.inner.tokens.stored_scopes(auth.character_id).await? {
            let merged: BTreeSet<String> = auth.scopes.union(&previous).cloned().collect();
            if merged != auth.scopes {
                match sso
                    .refresh_with_scopes(&auth.refresh_token, merged.iter().cloned())
                    .await
                {
                    Ok(widened) => {
                        if let Some(widened) = StoredAuth::from_tokens(&widened)
                            .filter(|w| w.character_id == auth.character_id)
                        {
                            auth = widened;
                        }
                    }
                    // EVE refused the wider scope set: keep this login's tokens.
                    Err(AuthError::Rejected { error, .. }) => {
                        tracing::debug!(character_id = auth.character_id, %error, "scope merge refused");
                    }
                    Err(e) => return Err(error::sso(e)),
                }
            }
        }

        self.inner.tokens.save(&auth).await?;
        Ok(AuthResult {
            character_id: auth.character_id,
            character_name: auth.character_name,
            scopes: auth.scopes,
        })
    }
}
