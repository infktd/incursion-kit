//! Live-event helpers shared by the waitlist routes (no routes).
//!
//! The legacy waitlist publishes `waitlist_update` (`{"waitlist_id": i64}`) on the `waitlist`
//! topic after most mutations, and on x-up first `message` (`{"message": "New x-up in
//! waitlist"}`) to the account of every registered fleet's boss. Every `waitlist_update` is now
//! paired with `status_update` (same data) on the public `status` topic, and it is also sent
//! where the legacy waitlist was silent but the waitlist's gate or status data changes
//! (emptying, invites, waitlist administration, fleets linked to a waitlist).
//!
//! The legacy waitlist sent `message` to `account;<boss character id>`, which nobody receives
//! when the boss is an alt (topics are keyed by account). It now goes to the boss's account.

use serde::Serialize;
use wl_live::{events, topics};

use crate::{ApiError, ApiResult, AppState};

#[derive(Serialize)]
struct Message {
    message: &'static str,
}

/// `waitlist_update` on the `waitlist` topic and `status_update` on the public `status` topic.
pub fn waitlist_update(state: &AppState, waitlist_id: i64) -> ApiResult<()> {
    state.live().waitlist_changed(waitlist_id);
    Ok(())
}

/// After an x-up: `message` to the account of every registered fleet's boss, then
/// `waitlist_update`. Like the legacy waitlist, failing to list the fleets only skips the
/// messages.
pub async fn waitlist_update_and_xup(state: &AppState, waitlist_id: i64) -> ApiResult<()> {
    let bosses: Result<Vec<i64>, _> = sqlx::query_scalar(
        "SELECT DISTINCT COALESCE(
            (SELECT MIN(account_id) FROM alt_character WHERE alt_id = fleet.boss_id),
            fleet.boss_id)
         FROM fleet",
    )
    .fetch_all(state.db())
    .await;
    match bosses {
        Ok(bosses) => {
            for boss_id in bosses {
                state
                    .live()
                    .publish_json(
                        &topics::account(boss_id),
                        events::MESSAGE,
                        &Message {
                            message: "New x-up in waitlist",
                        },
                    )
                    .map_err(ApiError::internal)?;
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not list fleets for x-up messages"),
    }
    waitlist_update(state, waitlist_id)
}
