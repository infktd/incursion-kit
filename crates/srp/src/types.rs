//! Wire types (the legacy waitlist's JSON shapes) and database rows.

use serde::{Deserialize, Serialize};

pub use wl_esi::wallet::CorporationWallet;

/// A loss report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SrpReport {
    pub killmail_id: i64,
    /// The ESI killmail URL (`""` in the FC list unless `show_fc_killmail_links` is set).
    pub killmail_link: String,
    /// Unix seconds.
    pub submitted_at: i64,
    pub loot_returned: bool,
    pub description: Option<String>,
    /// The FC who filed the report.
    pub submitted_by_id: i64,
    /// `Unknown` if the submitter has no character row.
    pub submitted_by_name: String,
    /// `pending`, `approved`, `rejected` or `paid`.
    pub status: String,
    /// ISK.
    pub payout_amount: Option<f64>,
    /// When the payout was detected in the wallet journal (Unix seconds).
    pub payout_date: Option<i64>,
    /// The coverage snapshot taken at submission, as a JSON-encoded string:
    /// `{"had_coverage": false}` or `{"had_coverage": true, "payment_date", "payment_amount",
    /// "coverage_character", "coverage_type"}`.
    pub srp_paid: Option<String>,
    /// Denial reason.
    pub reason: Option<String>,
    /// `null` for an NPC victim; `Unknown` if the name could not be resolved.
    pub victim_character_name: Option<String>,
    /// The lost ship's name (`Unknown` if it could not be resolved).
    pub victim_ship_type: Option<String>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct ReportRow {
    pub killmail_id: i64,
    pub killmail_link: String,
    pub submitted_at: i64,
    pub loot_returned: i8,
    pub description: Option<String>,
    pub submitted_by_id: i64,
    pub submitted_by_name: Option<String>,
    pub status: String,
    pub payout_amount: Option<f64>,
    pub payout_date: Option<i64>,
    pub srp_paid: Option<sqlx::types::Json<serde_json::Value>>,
    pub reason: Option<String>,
    pub victim_character_name: Option<String>,
    pub victim_ship_type: Option<String>,
}

impl From<ReportRow> for SrpReport {
    fn from(r: ReportRow) -> Self {
        Self {
            killmail_id: r.killmail_id,
            killmail_link: r.killmail_link,
            submitted_at: r.submitted_at,
            loot_returned: r.loot_returned > 0,
            description: r.description,
            submitted_by_id: r.submitted_by_id,
            submitted_by_name: r.submitted_by_name.unwrap_or_else(|| "Unknown".into()),
            status: r.status,
            payout_amount: r.payout_amount,
            payout_date: r.payout_date,
            srp_paid: r.srp_paid.map(|v| v.0.to_string()),
            reason: r.reason,
            victim_character_name: r.victim_character_name,
            victim_ship_type: r.victim_ship_type,
        }
    }
}

/// The columns of [`ReportRow`], selected from `srp_reports r` joined with the submitter `c`.
pub(crate) const REPORT_SELECT: &str = "SELECT r.killmail_id, r.killmail_link, r.submitted_at, \
     r.loot_returned, r.description, r.submitted_by_id, r.status, \
     CAST(r.payout_amount AS DOUBLE) AS payout_amount, r.payout_date, r.srp_paid, r.reason, \
     c.name AS submitted_by_name, r.victim_character_name, r.victim_ship_type \
     FROM srp_reports r LEFT JOIN `character` c ON r.submitted_by_id = c.id";

/// A detected subscription payment (`srp_payments`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct SrpPayment {
    pub id: i64,
    /// The depositor's name, as written in the wallet journal.
    pub character_name: String,
    /// ISK.
    pub payment_amount: f64,
    /// Unix seconds.
    pub payment_date: i64,
    /// Unix seconds.
    pub expires_at: i64,
    /// `daily` or `per_focus`.
    pub coverage_type: String,
    /// Unix seconds.
    pub created_at: i64,
    /// When the end of the incursion focus voided this per-focus payment (Unix seconds).
    pub focus_voided_at: Option<i64>,
}

/// The configured SRP service account (tokens are never returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceAccount {
    pub character_id: i64,
    pub character_name: String,
    pub corporation_id: i64,
    /// Wallet division.
    pub wallet_id: i32,
    pub is_active: bool,
    /// Last reconciliation run (Unix seconds).
    pub last_used: Option<i64>,
    /// Unix seconds.
    pub created_at: i64,
}

/// A wallet journal entry as the admin journal endpoint returns it (ESI's shape, with the
/// `*_party_type` fields the legacy waitlist declared; ESI no longer sends them, so they are
/// always `null`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WalletJournalEntry {
    pub amount: f64,
    pub balance: f64,
    pub context_id: Option<i64>,
    pub context_id_type: Option<String>,
    /// As ESI formats it, e.g. `2024-05-01T12:00:00Z`.
    pub date: String,
    pub description: String,
    pub first_party_id: Option<i64>,
    /// Always `null`.
    pub first_party_type: Option<String>,
    pub id: i64,
    pub reason: Option<String>,
    /// e.g. `player_donation`, `corporation_account_withdrawal`.
    pub ref_type: String,
    pub second_party_id: Option<i64>,
    /// Always `null`.
    pub second_party_type: Option<String>,
    pub tax: Option<f64>,
    pub tax_receiver_id: Option<i64>,
}

impl From<wl_esi::wallet::WalletJournalEntry> for WalletJournalEntry {
    fn from(e: wl_esi::wallet::WalletJournalEntry) -> Self {
        Self {
            amount: e.amount,
            balance: e.balance,
            context_id: e.context_id,
            context_id_type: e.context_id_type,
            date: e.date,
            description: e.description,
            first_party_id: e.first_party_id,
            first_party_type: None,
            id: e.id,
            reason: e.reason,
            ref_type: e.ref_type,
            second_party_id: e.second_party_id,
            second_party_type: None,
            tax: e.tax,
            tax_receiver_id: e.tax_receiver_id,
        }
    }
}
