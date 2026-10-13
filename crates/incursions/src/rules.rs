//! Timing rules: how long an incursion can still last and when the next one may spawn.
//!
//! **Maximum remaining time** (ported from eve-incursions.de, `spawn.tsx`, MIT): an incursion
//! lasts at most this long after it entered its current state:
//!
//! | state | at most | why |
//! |---|---|---|
//! | `established` | 8 days | up to 5 days established, 2 mobilizing, 1 withdrawing |
//! | `mobilizing` | 3 days | 2 days mobilizing, 1 withdrawing |
//! | `withdrawing` | 1 day | |
//!
//! **Spawn windows** (eve-incursions.de's high-sec banner, which eve-incursions.space shows per
//! security band): after an incursion of a band ends, no new one spawns in that band for
//! 12 hours; the replacement spawns within 36 hours of the end. The game keeps a fixed number of
//! incursions per band (`incursion_tracker.slots`, by default 1 high-sec, 1 low-sec and
//! 3 null-sec), so a band with `k` free slots gets one window for each of its `k` most recent
//! ends.

use chrono::{DateTime, Utc};
use serde::Serialize;

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// Earliest time after an end that a band's next incursion can spawn.
pub const WINDOW_OPENS_AFTER: i64 = 12 * HOUR;
/// Latest time after an end by which the band's next incursion spawns.
pub const WINDOW_CLOSES_AFTER: i64 = 36 * HOUR;

/// Longest an incursion can stay in `state` and the states after it; `None` for an unknown
/// state.
pub fn max_lifetime_after(state: &str) -> Option<i64> {
    match state {
        "established" => Some(8 * DAY),
        "mobilizing" => Some(3 * DAY),
        "withdrawing" => Some(DAY),
        _ => None,
    }
}

/// When an incursion that entered `state` at `since` will have despawned at the latest.
pub fn max_despawn_at(state: &str, since: i64) -> Option<i64> {
    max_lifetime_after(state).map(|lifetime| since + lifetime)
}

/// Where a spawn window stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowStatus {
    /// Too early: no spawn before `opens_at`.
    Pending,
    /// The spawn can happen any time until `closes_at`.
    Open,
    /// Past `closes_at` without a spawn: due any minute.
    Overdue,
}

/// The spawn window that an end at `ended_at` starts: `(opens_at, closes_at, status at now)`.
pub fn window(ended_at: i64, now: i64) -> (i64, i64, WindowStatus) {
    let opens = ended_at + WINDOW_OPENS_AFTER;
    let closes = ended_at + WINDOW_CLOSES_AFTER;
    let status = if now < opens {
        WindowStatus::Pending
    } else if now < closes {
        WindowStatus::Open
    } else {
        WindowStatus::Overdue
    };
    (opens, closes, status)
}

/// A Unix time as RFC 3339 (`2026-10-11T06:30:20Z`).
pub fn rfc3339(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifetimes_follow_the_state() {
        assert_eq!(max_despawn_at("established", 0), Some(8 * DAY));
        assert_eq!(max_despawn_at("mobilizing", 100), Some(100 + 3 * DAY));
        assert_eq!(max_despawn_at("withdrawing", 0), Some(DAY));
        assert_eq!(max_despawn_at("ended", 0), None);
    }

    #[test]
    fn windows_open_after_12_and_close_after_36_hours() {
        assert_eq!(window(0, 0), (12 * HOUR, 36 * HOUR, WindowStatus::Pending));
        assert_eq!(window(0, 12 * HOUR).2, WindowStatus::Open);
        assert_eq!(window(0, 36 * HOUR - 1).2, WindowStatus::Open);
        assert_eq!(window(0, 36 * HOUR).2, WindowStatus::Overdue);
        assert_eq!(rfc3339(1_791_640_502), "2026-10-10T13:55:02Z");
    }
}
