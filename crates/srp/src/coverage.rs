//! Who is covered, now or at a killmail's time. Coverage is bought per character **name** and
//! shared across an account: a character is covered by its own payment, its main's, or one of
//! its alts'.

use serde::Serialize;
use serde_json::json;
use wl_db::Db;

use crate::Srp;
use crate::error::SrpResult;
use crate::time::{minutes, now};

/// `GET /api/srp/status` (without the price factor the route adds).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PilotStatus {
    /// `Unpaid`, `Paid until end of focus` or `Paid until YYYY-MM-DD HH:MM UTC`.
    pub status: Option<String>,
    pub payment_amount: Option<f64>,
    pub coverage_type: Option<String>,
}

impl PilotStatus {
    fn unpaid() -> Self {
        Self {
            status: Some("Unpaid".into()),
            payment_amount: None,
            coverage_type: None,
        }
    }
}

/// Whether a killmail's victim was covered at the killmail's time
/// (`GET /api/admin/srp/reports/{id}/srp-validation`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct CoverageAtTime {
    pub had_srp_coverage: bool,
    /// `true` when the victim has no character row (the pilot must link the alt first).
    pub alt_needs_linking: Option<bool>,
    /// `YYYY-MM-DD HH:MM UTC`.
    pub payment_date: Option<String>,
    /// ISK.
    pub payment_amount: Option<f64>,
    /// `YYYY-MM-DD HH:MM UTC`.
    pub expires_at: Option<String>,
    /// The character whose payment covers the victim.
    pub coverage_character: Option<String>,
    /// `direct`, `main` or `alt`.
    pub coverage_type: Option<String>,
}

async fn name_of(db: &Db, character_id: i64) -> SrpResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM `character` WHERE id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await?,
    )
}

async fn main_of(db: &Db, character_id: i64) -> SrpResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT account_id FROM alt_character WHERE alt_id = ?")
            .bind(character_id)
            .fetch_optional(db)
            .await?,
    )
}

/// Names of `account_id`'s alts (except `except`), by id.
async fn alt_names(db: &Db, account_id: i64, except: Option<i64>) -> SrpResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT c.name FROM alt_character ac JOIN `character` c ON ac.alt_id = c.id \
         WHERE ac.account_id = ? AND ac.alt_id <> ? ORDER BY ac.alt_id",
    )
    .bind(account_id)
    .bind(except.unwrap_or(0))
    .fetch_all(db)
    .await?)
}

/// The names whose coverage covers `character_id`, with the coverage kind, in lookup order:
/// itself, its main (if it is an alt), then the account's other alts (or its own alts if it is
/// a main).
async fn coverage_candidates(
    db: &Db,
    character_id: i64,
    name: String,
) -> SrpResult<Vec<(String, &'static str)>> {
    let mut candidates = vec![(name, "direct")];
    match main_of(db, character_id).await? {
        Some(main_id) => {
            if let Some(main) = name_of(db, main_id).await? {
                candidates.push((main, "main"));
            }
            for alt in alt_names(db, main_id, Some(character_id)).await? {
                candidates.push((alt, "alt"));
            }
        }
        None => {
            for alt in alt_names(db, character_id, None).await? {
                candidates.push((alt, "alt"));
            }
        }
    }
    Ok(candidates)
}

#[derive(sqlx::FromRow)]
struct ActivePayment {
    expires_at: i64,
    coverage_type: String,
    payment_amount: f64,
}

#[derive(sqlx::FromRow)]
struct PaymentAt {
    payment_amount: f64,
    payment_amount_text: String,
    payment_date: i64,
    expires_at: i64,
}

impl Srp {
    /// A character's current coverage.
    pub async fn pilot_status(&self, character_id: i64) -> SrpResult<PilotStatus> {
        let db = self.db();
        let Some(name) = name_of(db, character_id).await? else {
            return Ok(PilotStatus::unpaid());
        };
        let now = now();
        for (name, _) in coverage_candidates(db, character_id, name).await? {
            let payment: Option<ActivePayment> = sqlx::query_as(
                "SELECT expires_at, coverage_type, CAST(payment_amount AS DOUBLE) AS payment_amount \
                 FROM srp_payments WHERE character_name = ? AND expires_at > ? \
                 ORDER BY expires_at DESC LIMIT 1",
            )
            .bind(&name)
            .bind(now)
            .fetch_optional(db)
            .await?;
            if let Some(p) = payment {
                let status = if p.coverage_type == "per_focus" {
                    "Paid until end of focus".to_string()
                } else {
                    format!("Paid until {}", minutes(p.expires_at))
                };
                return Ok(PilotStatus {
                    status: Some(status),
                    payment_amount: Some(p.payment_amount),
                    coverage_type: Some(p.coverage_type),
                });
            }
        }
        Ok(PilotStatus::unpaid())
    }

    /// The payment covering `name` at `timestamp`, if any. `respect_voiding` skips payments
    /// voided by the end of the focus (and treats `expires_at` as exclusive), as the submit-time
    /// snapshot does; the admin validation endpoint ignores voiding.
    async fn payment_at(
        &self,
        name: &str,
        timestamp: i64,
        respect_voiding: bool,
    ) -> SrpResult<Option<PaymentAt>> {
        let condition = if respect_voiding {
            "expires_at > ? AND focus_voided_at IS NULL"
        } else {
            "expires_at >= ?"
        };
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT CAST(payment_amount AS DOUBLE) AS payment_amount, \
             CAST(payment_amount AS CHAR) AS payment_amount_text, payment_date, expires_at \
             FROM srp_payments WHERE character_name = ? AND payment_date <= ? AND {condition} \
             ORDER BY payment_date DESC LIMIT 1"
        )))
        .bind(name)
        .bind(timestamp)
        .bind(timestamp)
        .fetch_optional(self.db())
        .await?)
    }

    /// Whether the victim was covered at `timestamp`, recomputed now (ignores focus voiding).
    pub async fn coverage_at(
        &self,
        victim_id: Option<i64>,
        timestamp: i64,
    ) -> SrpResult<CoverageAtTime> {
        let Some(victim_id) = victim_id else {
            return Ok(CoverageAtTime::default());
        };
        let Some(name) = name_of(self.db(), victim_id).await? else {
            return Ok(CoverageAtTime {
                alt_needs_linking: Some(true),
                ..Default::default()
            });
        };
        for (candidate, kind) in coverage_candidates(self.db(), victim_id, name).await? {
            if let Some(p) = self.payment_at(&candidate, timestamp, false).await? {
                return Ok(CoverageAtTime {
                    had_srp_coverage: true,
                    alt_needs_linking: None,
                    payment_date: Some(minutes(p.payment_date)),
                    payment_amount: Some(p.payment_amount),
                    expires_at: Some(minutes(p.expires_at)),
                    coverage_character: Some(candidate),
                    coverage_type: Some(kind.to_string()),
                });
            }
        }
        Ok(CoverageAtTime::default())
    }

    /// The `srp_paid` snapshot stored with a new report: the coverage at the killmail's time,
    /// honouring focus voiding. Checks the victim, its main, then the victim's own alts (as the
    /// legacy waitlist did; an alt victim is not covered by its main's other alts here).
    pub async fn coverage_snapshot(
        &self,
        victim_id: Option<i64>,
        timestamp: i64,
    ) -> SrpResult<serde_json::Value> {
        let not_covered = json!({"had_coverage": false});
        let Some(victim_id) = victim_id else {
            return Ok(not_covered);
        };
        let db = self.db();
        let Some(name) = name_of(db, victim_id).await? else {
            return Ok(not_covered);
        };
        let mut candidates = vec![(name, "direct")];
        if let Some(main_id) = main_of(db, victim_id).await?
            && let Some(main) = name_of(db, main_id).await?
        {
            candidates.push((main, "main"));
        }
        for alt in alt_names(db, victim_id, None).await? {
            candidates.push((alt, "alt"));
        }
        for (candidate, kind) in candidates {
            if let Some(p) = self.payment_at(&candidate, timestamp, true).await? {
                return Ok(json!({
                    "had_coverage": true,
                    "payment_date": minutes(p.payment_date),
                    "payment_amount": p.payment_amount_text,
                    "coverage_character": candidate,
                    "coverage_type": kind,
                }));
            }
        }
        Ok(not_covered)
    }
}
