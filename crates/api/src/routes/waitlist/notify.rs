//! Live-event helpers shared by the waitlist routes (no routes).
//!
//! The legacy waitlist publishes `waitlist_update` (`{"waitlist_id": i64}`) on the `waitlist`
//! topic after most mutations, and on x-up first `message` (`{"message": "New x-up in
//! waitlist"}`) to the account of every registered fleet's boss.
//!
//! The legacy waitlist sent `message` to `account;<boss character id>`, which nobody receives
//! when the boss is an alt (topics are keyed by account). It now goes to the boss's account.

use serde::Serialize;
use wl_live::{events, topics};

use crate::{ApiError, ApiResult, AppState};

#[derive(Serialize)]
struct WaitlistUpdate {
    waitlist_id: i64,
}

#[derive(Serialize)]
struct Message {
    message: &'static str,
}

/// `waitlist_update` on the `waitlist` topic.
pub fn waitlist_update(state: &AppState, waitlist_id: i64) -> ApiResult<()> {
    state
        .live()
        .publish_json(
            topics::WAITLIST,
            events::WAITLIST_UPDATE,
            &WaitlistUpdate { waitlist_id },
        )
        .map_err(ApiError::internal)
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
