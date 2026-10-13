//! Conversions from the generated ESI types to this crate's legacy-shaped ones.

use chrono::{DateTime, SecondsFormat, Utc};
use wl_core::types::TypeId;

use crate::error::{EsiError, EsiResult};

/// A generated string enum as the string ESI sent (`"squad_member"`, `"bounty_prizes"`, ...).
pub(crate) fn enum_str<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// A timestamp formatted as ESI sends it (`2024-05-01T12:00:00Z`).
pub(crate) fn esi_time(time: &DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn type_id(id: i64) -> EsiResult<TypeId> {
    TypeId::try_from(id).map_err(|_| EsiError::Http(format!("type ID {id} out of range").into()))
}

/// ESI accepts at most this many IDs in one bulk POST.
pub(crate) const BULK_LIMIT: usize = 1000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_enums_look_like_esi() {
        let time = DateTime::parse_from_rfc3339("2024-05-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(esi_time(&time), "2024-05-01T12:00:00Z");
        assert_eq!(
            enum_str(&eve_esi_client::types::FleetsFleetIdMembersGetItemRole::SquadMember),
            "squad_member"
        );
        assert!(type_id(i64::MAX).is_err());
        assert_eq!(type_id(17738).unwrap(), 17738);
    }
}
