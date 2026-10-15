//! Database-backed SSO tokens (`access_token` and `refresh_token` tables).
//!
//! Same rules as the legacy waitlist: a stored access token is used until its `expires`
//! (set to half the token's real lifetime); then the stored refresh token is exchanged, asking
//! for the stored scopes, and both rows are replaced with the result. EVE rotates refresh tokens,
//! so the new one is always written back. A refresh token EVE rejects as revoked/expired
//! (`invalid_grant`) is deleted with its access token and the call fails with
//! [`EsiError::NoToken`]: the character has to log in again. Other SSO failures (network, 5xx, a
//! misconfigured client secret) keep the tokens.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use eve_esi_client::auth::{AuthError, SsoClient, TokenProvider, TokenSet};
use wl_db::Db;

use crate::error::{self, EsiError, EsiResult};
use crate::esi::Esi;
use crate::scope::{Scope, join_scopes, split_scopes};

/// Tokens as stored for one character.
#[derive(Debug, Clone)]
pub(crate) struct StoredAuth {
    pub character_id: i64,
    pub character_name: String,
    pub access_token: String,
    /// Unix seconds after which the access token is refreshed.
    pub expires: i64,
    pub refresh_token: String,
    pub scopes: BTreeSet<String>,
}

impl StoredAuth {
    /// From a fresh token response. `None` if the access token doesn't identify a character or
    /// there's no refresh token.
    pub fn from_tokens(tokens: &TokenSet) -> Option<Self> {
        let claims = tokens.claims()?;
        Some(Self {
            character_id: i64::try_from(claims.character_id?).ok()?,
            character_name: claims.name.unwrap_or_default(),
            access_token: tokens.access_token.clone(),
            expires: half_life_expiry(tokens.expires_at),
            refresh_token: tokens.refresh_token.clone()?,
            scopes: claims.scopes.into_iter().collect(),
        })
    }
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The legacy waitlist treated an access token as expired halfway through its lifetime.
fn half_life_expiry(expires_at: Option<SystemTime>) -> i64 {
    let lifetime = expires_at
        .and_then(|at| at.duration_since(SystemTime::now()).ok())
        .map_or(20 * 60, |d| d.as_secs() as i64);
    now() + lifetime / 2
}

pub(crate) struct TokenStore {
    db: Db,
    sso: Arc<SsoClient>,
    /// One refresh at a time per character, so concurrent requests don't spend the same
    /// (rotating) refresh token twice.
    locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(sqlx::FromRow)]
struct AccessRow {
    access_token: String,
    expires: i64,
    scopes: String,
}

#[derive(sqlx::FromRow)]
struct RefreshRow {
    refresh_token: String,
    scopes: String,
}

impl TokenStore {
    pub fn new(db: Db, sso: Arc<SsoClient>) -> Self {
        Self {
            db,
            sso,
            locks: Mutex::default(),
        }
    }

    fn lock_for(&self, character_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        if locks.len() > 4096 {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        locks.entry(character_id).or_default().clone()
    }

    async fn fresh_access_token(
        &self,
        character_id: i64,
    ) -> EsiResult<Option<(String, BTreeSet<String>)>> {
        let row: Option<AccessRow> = sqlx::query_as(
            "SELECT access_token, expires, scopes FROM access_token WHERE character_id = ?",
        )
        .bind(character_id)
        .fetch_optional(&self.db)
        .await
        .map_err(error::db)?;
        Ok(row
            .filter(|row| row.expires >= now())
            .map(|row| (row.access_token, split_scopes(&row.scopes))))
    }

    /// A valid access token for `character_id` and the scopes it carries, refreshing it if the
    /// stored one has expired.
    pub async fn access_token_raw(
        &self,
        character_id: i64,
    ) -> EsiResult<(String, BTreeSet<String>)> {
        if let Some(fresh) = self.fresh_access_token(character_id).await? {
            return Ok(fresh);
        }
        let lock = self.lock_for(character_id);
        let _guard = lock.lock().await;
        // Another request may have refreshed while this one waited.
        if let Some(fresh) = self.fresh_access_token(character_id).await? {
            return Ok(fresh);
        }
        let refresh: Option<RefreshRow> = sqlx::query_as(
            "SELECT refresh_token, scopes FROM refresh_token WHERE character_id = ?",
        )
        .bind(character_id)
        .fetch_optional(&self.db)
        .await
        .map_err(error::db)?;
        let Some(refresh) = refresh else {
            return Err(EsiError::NoToken);
        };
        let scopes = split_scopes(&refresh.scopes);
        let tokens = match self
            .sso
            .refresh_with_scopes(&refresh.refresh_token, scopes.iter().cloned())
            .await
        {
            Ok(tokens) => tokens,
            Err(e) if e.is_revoked() => {
                tracing::warn!(character_id, error = %e, "deleting the refresh token EVE SSO rejected");
                self.delete(character_id).await?;
                return Err(EsiError::NoToken);
            }
            Err(e) => return Err(error::sso(e)),
        };
        let auth = StoredAuth::from_tokens(&tokens).ok_or_else(|| {
            EsiError::Http("EVE SSO returned a token that doesn't identify a character".into())
        })?;
        if auth.character_id != character_id {
            return Err(EsiError::Http(
                format!(
                    "refresh token stored for character {character_id} belongs to {}",
                    auth.character_id
                )
                .into(),
            ));
        }
        self.save(&auth).await?;
        Ok((auth.access_token, auth.scopes))
    }

    pub async fn access_token(&self, character_id: i64, scope: Scope) -> EsiResult<String> {
        let (token, scopes) = self.access_token_raw(character_id).await?;
        if !scopes.contains(scope.as_str()) {
            return Err(EsiError::MissingScope);
        }
        Ok(token)
    }

    /// The stored refresh token's scopes, if any.
    pub async fn stored_scopes(&self, character_id: i64) -> EsiResult<Option<BTreeSet<String>>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT scopes FROM refresh_token WHERE character_id = ?")
                .bind(character_id)
                .fetch_optional(&self.db)
                .await
                .map_err(error::db)?;
        Ok(row.map(|(scopes,)| split_scopes(&scopes)))
    }

    /// Store tokens (creating the `character` row if it's new), as the legacy `save_auth` did.
    pub async fn save(&self, auth: &StoredAuth) -> EsiResult<()> {
        let mut tx = self.db.begin().await.map_err(error::db)?;
        sqlx::query(
            "INSERT INTO `character` (id, name) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id",
        )
        .bind(auth.character_id)
        .bind(&auth.character_name)
        .execute(&mut *tx)
        .await
        .map_err(error::db)?;
        let scopes = join_scopes(&auth.scopes);
        sqlx::query(
            "REPLACE INTO access_token (character_id, access_token, expires, scopes) VALUES (?, ?, ?, ?)",
        )
        .bind(auth.character_id)
        .bind(&auth.access_token)
        .bind(auth.expires)
        .bind(&scopes)
        .execute(&mut *tx)
        .await
        .map_err(error::db)?;
        sqlx::query(
            "REPLACE INTO refresh_token (character_id, refresh_token, scopes) VALUES (?, ?, ?)",
        )
        .bind(auth.character_id)
        .bind(&auth.refresh_token)
        .bind(&scopes)
        .execute(&mut *tx)
        .await
        .map_err(error::db)?;
        tx.commit().await.map_err(error::db)
    }

    /// Mark the stored access token as expired, so the next call refreshes it.
    pub async fn expire(&self, character_id: i64) -> EsiResult<()> {
        sqlx::query("UPDATE access_token SET expires = 0 WHERE character_id = ?")
            .bind(character_id)
            .execute(&self.db)
            .await
            .map_err(error::db)?;
        Ok(())
    }

    async fn delete(&self, character_id: i64) -> EsiResult<()> {
        let mut tx = self.db.begin().await.map_err(error::db)?;
        for query in [
            "DELETE FROM access_token WHERE character_id = ?",
            "DELETE FROM refresh_token WHERE character_id = ?",
        ] {
            sqlx::query(query)
                .bind(character_id)
                .execute(&mut *tx)
                .await
                .map_err(error::db)?;
        }
        tx.commit().await.map_err(error::db)
    }
}

impl Esi {
    /// A valid access token for `character_id` with `scope` (refreshed and stored if needed).
    ///
    /// [`EsiError::NoToken`] if nothing usable is stored, [`EsiError::MissingScope`] if the
    /// token lacks `scope`.
    pub async fn access_token(&self, character_id: i64, scope: Scope) -> EsiResult<String> {
        self.inner.tokens.access_token(character_id, scope).await
    }

    /// The scopes granted to `character_id`'s stored token (refreshing it if expired).
    pub async fn granted_scopes(&self, character_id: i64) -> EsiResult<BTreeSet<String>> {
        Ok(self.inner.tokens.access_token_raw(character_id).await?.1)
    }

    /// Forget `character_id`'s current access token, so the next call refreshes it first: for a
    /// token ESI rejected (401) before it expired. A refresh token EVE has revoked then turns
    /// the next call into [`EsiError::NoToken`].
    pub async fn expire_access_token(&self, character_id: i64) -> EsiResult<()> {
        self.inner.tokens.expire(character_id).await
    }

    /// Whether a refresh token is stored for `character_id` (without contacting EVE).
    pub async fn has_token(&self, character_id: i64) -> EsiResult<bool> {
        Ok(self
            .inner
            .tokens
            .stored_scopes(character_id)
            .await?
            .is_some())
    }

    /// `character_id`'s stored tokens as an [`eve_esi_client::auth::TokenProvider`], checking
    /// `scope` on every use. For handing to code that takes a provider; this crate's own
    /// wrappers resolve tokens directly so failures keep their [`EsiError`] kind.
    pub fn token_provider(&self, character_id: i64, scope: Scope) -> Arc<dyn TokenProvider> {
        Arc::new(DbTokenProvider {
            esi: self.clone(),
            character_id,
            scope,
        })
    }
}

/// [`TokenProvider`] over the token tables for one character and scope.
struct DbTokenProvider {
    esi: Esi,
    character_id: i64,
    scope: Scope,
}

#[eve_esi_client::async_trait]
impl TokenProvider for DbTokenProvider {
    async fn access_token(&self) -> Result<String, AuthError> {
        self.esi
            .access_token(self.character_id, self.scope)
            .await
            .map_err(|e| match e {
                EsiError::NoToken => AuthError::NoRefreshToken,
                other => AuthError::Token(format!("ESI error: {other}")),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_is_half_the_lifetime() {
        let in_20_min = SystemTime::now() + std::time::Duration::from_secs(1200);
        let expires = half_life_expiry(Some(in_20_min));
        assert!((now() + 598..=now() + 600).contains(&expires), "{expires}");
        assert!(half_life_expiry(None) > now());
    }
}
