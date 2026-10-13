//! Wallet reconciliation: subscription deposits → `srp_payments`, payouts → `paid` reports.
//!
//! **Behaviour kept from the legacy waitlist** (documented because it is surprising):
//!
//! - Each run **wipes `srp_payments` and rebuilds it** from the journal entries of the last
//!   `srp.journal_days` (8) days. Payments older than that disappear, and per-focus voiding is
//!   re-applied afterwards by [`crate::focus::check_srp_validity`].
//! - Payers are identified by the **character name** parsed from the journal description
//!   (`"<name> deposited cash into …"`, `"<name> transferred cash from/to …"`), not by id. A
//!   renamed character, or one whose name is not in the description, is not matched.
//! - Payouts are outgoing entries; the recipient name is parsed from the description and matched,
//!   with the exact amount, against `approved` reports whose victim has that name and that were
//!   submitted within `srp.payout_match_days` (7) days before the payout.
//!
//! Differences: the wipe and rebuild happen in one transaction (the legacy wipe ran outside
//! it); deposits are de-duplicated per (time, amount, character) rather than per (time, amount);
//! a payout already credited to a report is not credited again to a second matching report; and
//! journal pages are read until the window is covered (the legacy waitlist read page 1 only).

use std::collections::HashMap;
use std::num::NonZeroU32;

use chrono::{DateTime, Duration, Utc};
use wl_db::Db;

use crate::error::{SrpError, SrpResult};
use crate::service_account;
use crate::tiers::coverage_for;
use crate::time::parse_rfc3339;
use crate::types::{CorporationWallet, ServiceAccount, SrpPayment, WalletJournalEntry};
use crate::{Srp, focus};

/// The depositor of an incoming journal entry.
pub fn deposit_character_name(description: &str) -> Option<String> {
    for marker in [
        " deposited cash into ",
        " transferred cash from ",
        " transferred cash to ",
    ] {
        if description.contains(marker) {
            let name = description.split(marker).next().unwrap_or_default();
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    None
}

/// The recipient of an outgoing journal entry (the legacy parsing, verbatim).
pub fn payout_character_name(description: &str) -> Option<String> {
    let before_account = |part: &str| {
        part.contains("'s account").then(|| {
            part.split("'s account")
                .next()
                .unwrap_or_default()
                .to_string()
        })
    };
    if description.contains(" withdrew cash from ") {
        description
            .split(" withdrew cash from ")
            .next()
            .map(str::to_string)
    } else if description.contains(" transferred cash from ") {
        description.split(" to ").nth(1).and_then(before_account)
    } else if description.contains(" transferred cash to ") {
        description
            .split(" transferred cash to ")
            .nth(1)
            .and_then(before_account)
    } else {
        None
    }
}

/// What a reconciliation run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// Payments now in `srp_payments`.
    pub payments: usize,
    /// Reports newly marked paid.
    pub paid_reports: Vec<i64>,
}

impl Srp {
    /// The service account, or 400 `No SRP service account configured`.
    pub async fn require_service_account(&self) -> SrpResult<ServiceAccount> {
        service_account::active(self.db())
            .await?
            .ok_or_else(|| SrpError::bad_request("No SRP service account configured"))
    }

    /// The service account's corporation wallets.
    pub async fn wallets(&self, account: &ServiceAccount) -> SrpResult<Vec<CorporationWallet>> {
        Ok(self
            .esi()
            .corporation_wallets(account.character_id, account.corporation_id)
            .await?)
    }

    /// Page 1 of a wallet division's journal (what the admin pages show). ESI failures are
    /// `BadRequest("ESI request failed: …")`, as in the legacy waitlist.
    pub async fn journal(
        &self,
        account: &ServiceAccount,
        division: i32,
    ) -> SrpResult<Vec<WalletJournalEntry>> {
        Ok(self
            .journal_page(account, division, NonZeroU32::MIN)
            .await?
            .0
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn journal_page(
        &self,
        account: &ServiceAccount,
        division: i32,
        page: NonZeroU32,
    ) -> SrpResult<(Vec<wl_esi::wallet::WalletJournalEntry>, u32)> {
        let page = self
            .esi()
            .corporation_wallet_journal(
                account.character_id,
                account.corporation_id,
                division,
                page,
            )
            .await
            .map_err(|e| SrpError::BadRequest(format!("ESI request failed: {e}")))?;
        Ok((page.entries, page.pages))
    }

    /// Journal entries newer than `since` (newest first), reading pages until one reaches past it.
    async fn journal_since(
        &self,
        account: &ServiceAccount,
        since: DateTime<Utc>,
    ) -> SrpResult<Vec<wl_esi::wallet::WalletJournalEntry>> {
        let mut entries = Vec::new();
        let mut page = NonZeroU32::MIN;
        loop {
            let (batch, pages) = self.journal_page(account, account.wallet_id, page).await?;
            let reached_window_start = batch
                .iter()
                .filter_map(|e| parse_rfc3339(&e.date))
                .any(|date| date < since);
            entries.extend(batch);
            if reached_window_start || page.get() >= pages {
                return Ok(entries);
            }
            page = page.saturating_add(1);
        }
    }

    /// Run a reconciliation: rebuild payments from the journal and mark matched payouts paid.
    /// Sends a Discord notice for every report newly marked paid. Per-focus voiding is not
    /// re-applied here; see [`Srp::process_and_validate`].
    pub async fn process_payments(&self) -> SrpResult<ProcessOutcome> {
        let account = self.require_service_account().await?;
        let tiers = &self.settings().tiers;
        let now = Utc::now();
        let since = now - Duration::days(tiers.journal_days);
        let entries = self.journal_since(&account, since).await?;
        if entries.is_empty() {
            // Nothing to rebuild from: keep the current payments (legacy behaviour).
            return Ok(ProcessOutcome::default());
        }

        let fc_factor = self.fc_price_factor();
        let mut fc_eligible: HashMap<String, bool> = HashMap::new();
        let mut outcome = ProcessOutcome::default();
        let mut tx = self.db().begin().await?;
        sqlx::query("DELETE FROM srp_payments")
            .execute(&mut *tx)
            .await?;

        for entry in &entries {
            let date = parse_rfc3339(&entry.date)
                .ok_or_else(|| SrpError::bad_request("Invalid date format"))?;
            if date < since {
                continue;
            }
            if entry.amount > 0.0 {
                let Some(name) = deposit_character_name(&entry.description) else {
                    continue;
                };
                let exists: Option<i64> = sqlx::query_scalar(
                    "SELECT id FROM srp_payments WHERE payment_date = ? AND payment_amount = ? \
                     AND character_name = ?",
                )
                .bind(date.timestamp())
                .bind(entry.amount)
                .bind(&name)
                .fetch_optional(&mut *tx)
                .await?;
                if exists.is_some() {
                    continue;
                }
                let eligible = match fc_eligible.get(&name) {
                    Some(eligible) => *eligible,
                    None => {
                        let eligible = self.qualifies_for_fc_price(&name).await?;
                        fc_eligible.insert(name.clone(), eligible);
                        eligible
                    }
                };
                let Some(coverage) = coverage_for(tiers, entry.amount, date, eligible, fc_factor)
                else {
                    continue;
                };
                sqlx::query(
                    "INSERT INTO srp_payments (character_name, payment_amount, payment_date, \
                     expires_at, coverage_type, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&name)
                .bind(entry.amount)
                .bind(date.timestamp())
                .bind(coverage.expires_at.timestamp())
                .bind(coverage.kind.as_str())
                .bind(now.timestamp())
                .execute(&mut *tx)
                .await?;
                outcome.payments += 1;
            } else if entry.amount < 0.0 {
                let Some(name) = payout_character_name(&entry.description) else {
                    continue;
                };
                if let Some(killmail_id) = match_payout(
                    &mut tx,
                    &name,
                    entry.amount.abs(),
                    date,
                    tiers.payout_match_days,
                )
                .await?
                {
                    outcome.paid_reports.push(killmail_id);
                }
            }
        }

        sqlx::query("UPDATE srp_service_account SET last_used = ? WHERE character_id = ?")
            .bind(now.timestamp())
            .bind(account.character_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        for killmail_id in &outcome.paid_reports {
            tracing::info!(killmail_id, "SRP report paid");
            self.spawn_notify_report(*killmail_id);
        }
        Ok(outcome)
    }

    /// [`Srp::process_payments`], then [`focus::check_srp_validity`] (what the updater runs).
    pub async fn process_and_validate(&self) -> SrpResult<ProcessOutcome> {
        let outcome = self.process_payments().await?;
        focus::check_srp_validity(self.db()).await?;
        Ok(outcome)
    }

    /// Whether the named character, or its main, holds a role that may pay the FC price.
    async fn qualifies_for_fc_price(&self, character_name: &str) -> SrpResult<bool> {
        let roles = &self.settings().tiers.fc_discount_roles;
        let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM `character` WHERE name = ?")
            .bind(character_name)
            .fetch_optional(self.db())
            .await?
        else {
            return Ok(false);
        };
        let main: Option<i64> =
            sqlx::query_scalar("SELECT account_id FROM alt_character WHERE alt_id = ?")
                .bind(id)
                .fetch_optional(self.db())
                .await?;
        for character_id in std::iter::once(id).chain(main) {
            let role: Option<String> =
                sqlx::query_scalar("SELECT role FROM admin WHERE character_id = ?")
                    .bind(character_id)
                    .fetch_optional(self.db())
                    .await?;
            if role.is_some_and(|r| roles.contains(&r)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Every payment, latest expiry first.
    pub async fn all_payments(&self) -> SrpResult<Vec<SrpPayment>> {
        Ok(sqlx::query_as(
            "SELECT id, character_name, CAST(payment_amount AS DOUBLE) AS payment_amount, \
             payment_date, expires_at, coverage_type, created_at, focus_voided_at \
             FROM srp_payments ORDER BY expires_at DESC",
        )
        .fetch_all(self.db())
        .await?)
    }
}

/// Mark the approved report this payout pays as paid; returns its killmail id.
async fn match_payout(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    character_name: &str,
    amount: f64,
    date: DateTime<Utc>,
    match_days: i64,
) -> SrpResult<Option<i64>> {
    let paid_at = date.timestamp();
    // This payout already paid a report (in an earlier run): don't let it pay another one.
    let already: Option<i64> = sqlx::query_scalar(
        "SELECT killmail_id FROM srp_reports WHERE status = 'paid' AND payout_date = ? \
         AND payout_amount = ? AND victim_character_name = ? LIMIT 1",
    )
    .bind(paid_at)
    .bind(amount)
    .bind(character_name)
    .fetch_optional(&mut **tx)
    .await?;
    if already.is_some() {
        return Ok(None);
    }
    let report: Option<i64> = sqlx::query_scalar(
        "SELECT killmail_id FROM srp_reports WHERE status = 'approved' AND payout_amount = ? \
         AND submitted_at >= ? AND submitted_at <= ? AND victim_character_name = ? \
         ORDER BY submitted_at LIMIT 1",
    )
    .bind(amount)
    .bind(paid_at - match_days * 24 * 3600)
    .bind(paid_at)
    .bind(character_name)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(killmail_id) = report else {
        return Ok(None);
    };
    sqlx::query("UPDATE srp_reports SET status = 'paid', payout_date = ? WHERE killmail_id = ?")
        .bind(paid_at)
        .bind(killmail_id)
        .execute(&mut **tx)
        .await?;
    Ok(Some(killmail_id))
}

/// The `srp_config` key/value map.
pub async fn config_map(db: &Db) -> SrpResult<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT `key`, value FROM srp_config")
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Upsert an `srp_config` entry.
pub async fn set_config(db: &Db, key: &str, value: &str, updated_by_id: i64) -> SrpResult<()> {
    sqlx::query(
        "INSERT INTO srp_config (`key`, value, updated_at, updated_by_id) VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE value = VALUES(value), updated_at = VALUES(updated_at), \
         updated_by_id = VALUES(updated_by_id)",
    )
    .bind(key)
    .bind(value)
    .bind(crate::time::now())
    .bind(updated_by_id)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_depositors() {
        assert_eq!(
            deposit_character_name("Some Pilot deposited cash into Corp's account").as_deref(),
            Some("Some Pilot")
        );
        assert_eq!(
            deposit_character_name("Some Pilot transferred cash from Some Pilot's account")
                .as_deref(),
            Some("Some Pilot")
        );
        assert_eq!(
            deposit_character_name("Alt X transferred cash to Corp's account").as_deref(),
            Some("Alt X")
        );
        assert_eq!(deposit_character_name("Bounty prizes"), None);
        assert_eq!(deposit_character_name(" deposited cash into x"), None);
    }

    #[test]
    fn parses_payout_recipients() {
        assert_eq!(
            payout_character_name("Director withdrew cash from Corp's account").as_deref(),
            Some("Director")
        );
        assert_eq!(
            payout_character_name(
                "Corp transferred cash from Corp's account to Lost Pilot's account"
            )
            .as_deref(),
            Some("Lost Pilot")
        );
        assert_eq!(
            payout_character_name("Corp transferred cash to Lost Pilot's account").as_deref(),
            Some("Lost Pilot")
        );
        assert_eq!(
            payout_character_name("Corp transferred cash to nowhere"),
            None
        );
        assert_eq!(payout_character_name("Market escrow"), None);
    }
}
