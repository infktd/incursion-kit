//! The SRP service account: the character whose ESI token reads the corporation wallet.
//!
//! It is set up through EVE SSO with `state=srp_setup` (`GET /api/admin/srp/setup` builds the
//! login URL; the auth callback then calls [`store_service_account_tokens`]). As in the legacy
//! waitlist, the tokens are *copied* into `srp_service_account`, but ESI calls are made with the
//! character's regular stored token (`access_token`/`refresh_token` tables), which the ESI client
//! keeps refreshed.

use wl_db::Db;

use crate::time::now;
use crate::types::ServiceAccount;

/// Scopes recorded for the service account (the legacy waitlist's values).
pub const SERVICE_ACCOUNT_SCOPES: &str = "esi-publicdata.v1 esi-wallet.read_corporation_wallets.v1";

/// Wallet division recorded at setup (the master wallet).
pub const DEFAULT_WALLET_DIVISION: i32 = 1;

/// What the SSO callback stores for the service account.
#[derive(Debug, Clone, Copy)]
pub struct ServiceAccountTokens<'a> {
    pub character_id: i64,
    pub character_name: &'a str,
    pub corporation_id: i64,
    pub wallet_id: i32,
    pub access_token: &'a str,
    pub refresh_token: &'a str,
    pub expires: i64,
    pub scopes: &'a str,
}

/// Create or update the service account row for `tokens.character_id` (the legacy upsert).
pub async fn store_service_account_tokens(
    db: &Db,
    tokens: &ServiceAccountTokens<'_>,
) -> Result<(), sqlx::Error> {
    let now = now();
    sqlx::query(
        "INSERT INTO srp_service_account (
            character_id, character_name, corporation_id, wallet_id,
            access_token, refresh_token, expires, scopes, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON DUPLICATE KEY UPDATE
            character_name = VALUES(character_name),
            corporation_id = VALUES(corporation_id),
            wallet_id = VALUES(wallet_id),
            access_token = VALUES(access_token),
            refresh_token = VALUES(refresh_token),
            expires = VALUES(expires),
            scopes = VALUES(scopes),
            updated_at = VALUES(updated_at)",
    )
    .bind(tokens.character_id)
    .bind(tokens.character_name)
    .bind(tokens.corporation_id)
    .bind(tokens.wallet_id)
    .bind(tokens.access_token)
    .bind(tokens.refresh_token)
    .bind(tokens.expires)
    .bind(tokens.scopes)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    character_id: i64,
    character_name: String,
    corporation_id: i64,
    wallet_id: i32,
    is_active: i8,
    last_used: Option<i64>,
    created_at: i64,
}

/// The active service account, if any.
pub async fn active(db: &Db) -> Result<Option<ServiceAccount>, sqlx::Error> {
    let row: Option<AccountRow> = sqlx::query_as(
        "SELECT character_id, character_name, corporation_id, wallet_id, is_active, last_used, \
         created_at FROM srp_service_account WHERE is_active = 1 LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| ServiceAccount {
        character_id: r.character_id,
        character_name: r.character_name,
        corporation_id: r.corporation_id,
        wallet_id: r.wallet_id,
        is_active: r.is_active > 0,
        last_used: r.last_used,
        created_at: r.created_at,
    }))
}

/// Delete every service account row.
pub async fn remove_all(db: &Db) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM srp_service_account")
        .execute(db)
        .await?;
    Ok(())
}
