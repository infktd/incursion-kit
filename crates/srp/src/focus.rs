//! The incursion focus (`incursion_focus`, written by the incursion updater) and the per-focus
//! coverage it ends.

use serde::{Deserialize, Serialize};
use wl_db::Db;

use crate::time::now;

/// The latest `incursion_focus` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IncursionFocus {
    pub id: i32,
    pub current_focus_constellation_id: Option<i64>,
    pub current_focus_constellation_name: Option<String>,
    /// Unix seconds.
    pub last_check_timestamp: i64,
    pub focus_active: bool,
    /// When the last focus ended (Unix seconds).
    pub focus_end_timestamp: Option<i64>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct FocusRow {
    id: i32,
    current_focus_constellation_id: Option<i64>,
    current_focus_constellation_name: Option<String>,
    last_check_timestamp: i64,
    focus_active: i8,
    focus_end_timestamp: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

/// The current focus status (the latest row), if any.
pub async fn current_focus(db: &Db) -> Result<Option<IncursionFocus>, sqlx::Error> {
    let row: Option<FocusRow> = sqlx::query_as(
        "SELECT id, current_focus_constellation_id, current_focus_constellation_name, \
         last_check_timestamp, focus_active, focus_end_timestamp, created_at, updated_at \
         FROM incursion_focus ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| IncursionFocus {
        id: r.id,
        current_focus_constellation_id: r.current_focus_constellation_id,
        current_focus_constellation_name: r.current_focus_constellation_name,
        last_check_timestamp: r.last_check_timestamp,
        focus_active: r.focus_active > 0,
        focus_end_timestamp: r.focus_end_timestamp,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }))
}

/// Void the per-focus coverage bought before the last focus ended: such payments expire now and
/// get `focus_voided_at = now`. Returns the number of payments voided.
pub async fn check_srp_validity(db: &Db) -> Result<u64, sqlx::Error> {
    let Some(focus_end) = current_focus(db).await?.and_then(|f| f.focus_end_timestamp) else {
        return Ok(0);
    };
    let now = now();
    let result = sqlx::query(
        "UPDATE srp_payments SET expires_at = ?, focus_voided_at = ? \
         WHERE focus_voided_at IS NULL AND coverage_type = 'per_focus' AND payment_date < ?",
    )
    .bind(now)
    .bind(now)
    .bind(focus_end)
    .execute(db)
    .await?;
    if result.rows_affected() > 0 {
        tracing::info!(
            voided = result.rows_affected(),
            "voided per-focus SRP payments: the focus ended"
        );
    }
    Ok(result.rows_affected())
}

/// The latest focus end time and its `YYYY-MM-DD HH:MM:SS UTC` rendering.
pub async fn focus_end(db: &Db) -> Result<(Option<i64>, Option<String>), sqlx::Error> {
    let end = current_focus(db).await?.and_then(|f| f.focus_end_timestamp);
    Ok((end, end.map(crate::time::seconds)))
}
