//! Corporation wallets (the SRP service account's `esi-wallet.read_corporation_wallets.v1`).

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorStyle, EsiResult};
use crate::esi::{Esi, send};
use crate::scope::Scope;
use crate::util::{enum_str, esi_time};

/// A corporation wallet division's balance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CorporationWallet {
    /// Wallet division, 1 (master wallet) to 7.
    pub division: i32,
    /// ISK.
    pub balance: f64,
}

/// A wallet journal entry, shaped as the legacy waitlist stored it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalletJournalEntry {
    /// 0 when ESI omits it.
    pub amount: f64,
    /// 0 when ESI omits it.
    pub balance: f64,
    pub context_id: Option<i64>,
    pub context_id_type: Option<String>,
    /// As ESI formats it, e.g. `2024-05-01T12:00:00Z`.
    pub date: String,
    pub description: String,
    pub first_party_id: Option<i64>,
    pub id: i64,
    pub reason: Option<String>,
    /// e.g. `player_donation`, `corporation_account_withdrawal`.
    pub ref_type: String,
    pub second_party_id: Option<i64>,
    pub tax: Option<f64>,
    pub tax_receiver_id: Option<i64>,
}

/// One page of a wallet journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalPage {
    pub entries: Vec<WalletJournalEntry>,
    /// Total pages (ESI's `X-Pages`), 1 if it didn't say.
    pub pages: u32,
}

impl Esi {
    /// Balances of `corporation_id`'s wallet divisions, read as `character_id`.
    pub async fn corporation_wallets(
        &self,
        character_id: i64,
        corporation_id: i64,
    ) -> EsiResult<Vec<CorporationWallet>> {
        let client = self
            .client_for(character_id, Scope::WalletReadCorporationWallets)
            .await?;
        let wallets = send(
            client
                .get_corporations_corporation_id_wallets()
                .corporation_id(corporation_id)
                .send(),
            ErrorStyle::Body,
        )
        .await?;
        Ok(wallets
            .0
            .into_iter()
            .map(|w| CorporationWallet {
                division: i32::try_from(w.division).unwrap_or_default(),
                balance: w.balance,
            })
            .collect())
    }

    /// One page (from 1, newest first) of a corporation wallet division's journal, read as
    /// `character_id`. The legacy waitlist only ever read page 1.
    pub async fn corporation_wallet_journal(
        &self,
        character_id: i64,
        corporation_id: i64,
        division: i32,
        page: NonZeroU32,
    ) -> EsiResult<JournalPage> {
        let client = self
            .client_for(character_id, Scope::WalletReadCorporationWallets)
            .await?;
        let response = client
            .get_corporations_corporation_id_wallets_division_journal()
            .corporation_id(corporation_id)
            .division(i64::from(division))
            .page(page)
            .send()
            .await;
        let pages = response
            .as_ref()
            .ok()
            .and_then(|r| r.headers().get("x-pages"))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let journal = send(async { response }, ErrorStyle::Body).await?;
        Ok(JournalPage {
            entries: journal
                .0
                .into_iter()
                .map(|e| WalletJournalEntry {
                    amount: e.amount.unwrap_or_default(),
                    balance: e.balance.unwrap_or_default(),
                    context_id: e.context_id,
                    context_id_type: e.context_id_type.as_ref().map(enum_str),
                    date: esi_time(&e.date),
                    description: e.description,
                    first_party_id: e.first_party_id,
                    id: e.id,
                    reason: e.reason,
                    ref_type: enum_str(&e.ref_type),
                    second_party_id: e.second_party_id,
                    tax: e.tax,
                    tax_receiver_id: e.tax_receiver_id,
                })
                .collect(),
            pages,
        })
    }
}
