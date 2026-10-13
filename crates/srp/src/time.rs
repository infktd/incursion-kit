//! Timestamp helpers (all SRP times are Unix seconds, UTC).

use chrono::{DateTime, Utc};

pub(crate) fn now() -> i64 {
    Utc::now().timestamp()
}

pub(crate) fn utc(timestamp: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(timestamp, 0).unwrap_or_default()
}

/// `YYYY-MM-DD HH:MM UTC`, the format of the SRP status texts.
pub(crate) fn minutes(timestamp: i64) -> String {
    utc(timestamp).format("%Y-%m-%d %H:%M UTC").to_string()
}

/// `YYYY-MM-DD HH:MM:SS UTC`.
pub(crate) fn seconds(timestamp: i64) -> String {
    utc(timestamp).format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// Parse an ESI (RFC 3339) time.
pub(crate) fn parse_rfc3339(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}
