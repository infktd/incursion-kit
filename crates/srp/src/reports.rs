//! Loss reports: submission by FCs, edits, admin decisions.

use std::collections::BTreeMap;

use crate::Srp;
use crate::error::{SrpError, SrpResult};
use crate::killmail::parse_link;
use crate::time::{now, parse_rfc3339};
use crate::types::{REPORT_SELECT, ReportRow, SrpReport};

/// The killmail time as Unix seconds (`BadRequest` if ESI sent something unparsable).
pub fn killmail_timestamp(killmail_time: &str) -> SrpResult<i64> {
    parse_rfc3339(killmail_time)
        .map(|t| t.timestamp())
        .ok_or_else(|| SrpError::bad_request("Invalid killmail time format"))
}

impl Srp {
    /// Every report, newest first.
    pub async fn all_reports(&self) -> SrpResult<Vec<SrpReport>> {
        let rows: Vec<ReportRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{REPORT_SELECT} ORDER BY r.submitted_at DESC"
        )))
        .fetch_all(self.db())
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn report(&self, killmail_id: i64) -> SrpResult<Option<SrpReport>> {
        let row: Option<ReportRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{REPORT_SELECT} WHERE r.killmail_id = ?"
        )))
        .bind(killmail_id)
        .fetch_optional(self.db())
        .await?;
        Ok(row.map(Into::into))
    }

    /// The report, or 404 `SRP report not found`.
    pub async fn require_report(&self, killmail_id: i64) -> SrpResult<SrpReport> {
        self.report(killmail_id)
            .await?
            .ok_or_else(SrpError::report_not_found)
    }

    /// Who submitted a report (404 if there is no such report).
    pub async fn report_submitter(&self, killmail_id: i64) -> SrpResult<i64> {
        sqlx::query_scalar("SELECT submitted_by_id FROM srp_reports WHERE killmail_id = ?")
            .bind(killmail_id)
            .fetch_optional(self.db())
            .await?
            .ok_or_else(SrpError::report_not_found)
    }

    /// File a report for a killmail: snapshots the victim's coverage and the fleet composition,
    /// stores it as `pending` and posts the Discord notice.
    pub async fn submit_report(
        &self,
        killmail_link: &str,
        description: &str,
        loot_returned: bool,
        submitted_by_id: i64,
    ) -> SrpResult<()> {
        let (killmail_id, _) = parse_link(killmail_link)
            .map_err(|e| SrpError::BadRequest(format!("Invalid killmail link: {e}")))?;
        let km = self.killmail(killmail_link).await?;
        let timestamp = killmail_timestamp(&km.killmail_time)?;

        let victim_name = match km.victim.character_id {
            None => None, // NPC
            Some(id) => {
                let stored: Option<String> =
                    sqlx::query_scalar("SELECT name FROM `character` WHERE id = ?")
                        .bind(id)
                        .fetch_optional(self.db())
                        .await?;
                Some(match stored {
                    Some(name) => name,
                    None => tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        self.esi().character(id),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map_or_else(|| "Unknown".to_string(), |c| c.name),
                })
            }
        };
        let ship_type = self
            .type_name(km.victim.ship_type_id, 5)
            .await
            .unwrap_or_else(|| "Unknown".into());
        let srp_paid = self
            .coverage_snapshot(km.victim.character_id, timestamp)
            .await?;
        let fleet_comp = self
            .fleet_composition(timestamp, km.victim.character_id)
            .await?;

        let existing: Option<i64> =
            sqlx::query_scalar("SELECT killmail_id FROM srp_reports WHERE killmail_id = ?")
                .bind(killmail_id)
                .fetch_optional(self.db())
                .await?;
        if existing.is_some() {
            return Err(SrpError::bad_request(
                "An SRP report for this killmail already exists. Use the update button to modify existing reports.",
            ));
        }

        sqlx::query(
            "INSERT INTO srp_reports (killmail_id, killmail_link, submitted_at, loot_returned, \
             description, submitted_by_id, status, victim_character_name, victim_ship_type, \
             srp_paid, fleet_comp) VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(killmail_id)
        .bind(killmail_link)
        .bind(now())
        .bind(loot_returned)
        .bind(description)
        .bind(submitted_by_id)
        .bind(victim_name)
        .bind(ship_type)
        .bind(srp_paid.to_string())
        .bind(fleet_comp.map(|v| v.to_string()))
        .execute(self.db())
        .await?;

        self.spawn_notify_report(killmail_id);
        Ok(())
    }

    /// Edit a report's description and loot flag, recomputing its fleet composition. The
    /// status is unchanged and no notice is sent.
    pub async fn update_report(
        &self,
        killmail_id: i64,
        description: &str,
        loot_returned: bool,
    ) -> SrpResult<()> {
        let link: String =
            sqlx::query_scalar("SELECT killmail_link FROM srp_reports WHERE killmail_id = ?")
                .bind(killmail_id)
                .fetch_optional(self.db())
                .await?
                .ok_or_else(SrpError::report_not_found)?;
        let km = self.killmail(&link).await?;
        let timestamp = killmail_timestamp(&km.killmail_time)?;
        let fleet_comp = self
            .fleet_composition(timestamp, km.victim.character_id)
            .await?;
        sqlx::query(
            "UPDATE srp_reports SET description = ?, loot_returned = ?, fleet_comp = ? \
             WHERE killmail_id = ?",
        )
        .bind(description)
        .bind(loot_returned)
        .bind(fleet_comp.map(|v| v.to_string()))
        .bind(killmail_id)
        .execute(self.db())
        .await?;
        Ok(())
    }

    /// Approve a report for `payout_amount` ISK. No ISK moves: the payout is made in game and
    /// detected by the next reconciliation.
    pub async fn approve_report(&self, killmail_id: i64, payout_amount: f64) -> SrpResult<()> {
        self.report_submitter(killmail_id).await?;
        self.ensure_not_paid(killmail_id).await?;
        sqlx::query(
            "UPDATE srp_reports SET status = 'approved', payout_amount = ? WHERE killmail_id = ?",
        )
        .bind(payout_amount)
        .bind(killmail_id)
        .execute(self.db())
        .await?;
        self.spawn_notify_report(killmail_id);
        Ok(())
    }

    pub async fn deny_report(&self, killmail_id: i64, reason: &str) -> SrpResult<()> {
        self.report_submitter(killmail_id).await?;
        self.ensure_not_paid(killmail_id).await?;
        sqlx::query("UPDATE srp_reports SET status = 'rejected', reason = ? WHERE killmail_id = ?")
            .bind(reason)
            .bind(killmail_id)
            .execute(self.db())
            .await?;
        self.spawn_notify_report(killmail_id);
        Ok(())
    }

    /// A paid report's decision is final: the ISK has already moved, so approving or denying it
    /// again would leave the record contradicting the wallet. (The legacy waitlist allowed it.)
    async fn ensure_not_paid(&self, killmail_id: i64) -> SrpResult<()> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM srp_reports WHERE killmail_id = ?")
                .bind(killmail_id)
                .fetch_optional(self.db())
                .await?;
        if status.as_deref() == Some("paid") {
            return Err(SrpError::bad_request("SRP report is already paid"));
        }
        Ok(())
    }

    /// Hull counts (`{ship name: count}`) of everyone in a tracked fleet at `timestamp`, or
    /// `None` when the victim wasn't in one (or nobody was).
    pub async fn fleet_composition(
        &self,
        timestamp: i64,
        victim_id: Option<i64>,
    ) -> SrpResult<Option<serde_json::Value>> {
        if let Some(victim_id) = victim_id {
            let in_fleet: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM fleet_activity WHERE character_id = ? AND first_seen <= ? \
                 AND last_seen >= ? AND has_left = 0 LIMIT 1",
            )
            .bind(victim_id)
            .bind(timestamp)
            .bind(timestamp)
            .fetch_optional(self.db())
            .await?;
            if in_fleet.is_none() {
                return Ok(None);
            }
        }
        let hulls: Vec<i32> = sqlx::query_scalar(
            "SELECT hull FROM fleet_activity WHERE first_seen <= ? AND last_seen >= ? \
             AND has_left = 0",
        )
        .bind(timestamp)
        .bind(timestamp)
        .fetch_all(self.db())
        .await?;
        if hulls.is_empty() {
            return Ok(None);
        }
        let mut counts: BTreeMap<i32, i64> = BTreeMap::new();
        for hull in hulls {
            *counts.entry(hull).or_default() += 1;
        }
        let mut comp = serde_json::Map::new();
        for (hull, count) in counts {
            let name = self
                .type_name(i64::from(hull), 2)
                .await
                .unwrap_or_else(|| format!("Ship_{hull}"));
            comp.insert(name, count.into());
        }
        Ok(Some(serde_json::Value::Object(comp)))
    }

    /// Reports whose victim is `character_id`, its alts or its main (by victim name). Only used
    /// by the `/api/pilot/srp-reports/{id}` endpoint, which the legacy waitlist never mounted.
    pub async fn reports_for_pilot(&self, character_id: i64) -> SrpResult<Vec<SrpReport>> {
        let rows: Vec<ReportRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{REPORT_SELECT} WHERE r.victim_character_name IN ( \
               SELECT name FROM `character` WHERE id = ? \
               OR id IN (SELECT alt_id FROM alt_character WHERE account_id = ?) \
               OR id IN (SELECT account_id FROM alt_character WHERE alt_id = ?)) \
             ORDER BY r.submitted_at DESC"
        )))
        .bind(character_id)
        .bind(character_id)
        .bind(character_id)
        .fetch_all(self.db())
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
