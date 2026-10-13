//! Roles and the permission keys they grant.
//!
//! Roles are cumulative: each grants everything the previous one does, plus its own keys. The
//! role is stored in `admin.role` for the account's main character; no row means `user`. The
//! table is immutable and built once.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

/// A role's effective permission keys, sorted (the order `whoami` reports them in).
pub type AccessSet = BTreeSet<&'static str>;

/// Role names in ascending order of privilege.
pub const ROLES: [&str; 6] = ["user", "trainee", "trainee-advanced", "fc", "fc+", "admin"];

/// Keys each role adds on top of the previous role in [`ROLES`].
const ROLE_GRANTS: [&[&str]; 6] = [
    &[],
    &[
        "fleet-configure",
        "fleet-invite",
        "fleet-view",
        "pilot-view",
        "waitlist-view",
        "waitlist-tag:TRAINEE",
    ],
    &["fit-view", "skill-view", "waitlist-manage"],
    &[
        "bans-manage",
        "badges-manage",
        "commanders-view",
        "fleet-activity-view",
        "fleet-history-view",
        "fit-history-view",
        "search",
        "skill-history-view",
        "waitlist-edit",
        "stats-view",
        "waitlist-tag:HQ-FC",
        "notes-view",
        "notes-add",
    ],
    &[
        "commanders-manage",
        "commanders-manage:trainee",
        "commanders-manage:trainee-advanced",
        "commanders-manage:fc",
    ],
    &["commanders-manage:admin", "commanders-manage:fc+"],
];

static ACCESS_LEVELS: LazyLock<BTreeMap<&'static str, AccessSet>> =
    LazyLock::new(build_access_levels);

/// Build the role → permission-key table.
pub fn build_access_levels() -> BTreeMap<&'static str, AccessSet> {
    let mut levels = BTreeMap::new();
    let mut current = AccessSet::new();
    for (role, grants) in ROLES.iter().zip(ROLE_GRANTS) {
        current.extend(grants.iter().copied());
        levels.insert(*role, current.clone());
    }
    levels
}

/// The permission keys of `role`, or `None` for an unknown role.
pub fn access_keys(role: &str) -> Option<&'static AccessSet> {
    ACCESS_LEVELS.get(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_cumulative() {
        let levels = build_access_levels();
        assert!(levels["user"].is_empty());
        assert_eq!(levels["trainee"].len(), 6);
        assert_eq!(levels["trainee-advanced"].len(), 9);
        assert_eq!(levels["fc"].len(), 22);
        assert_eq!(levels["fc+"].len(), 26);
        assert_eq!(levels["admin"].len(), 28);
        for pair in ROLES.windows(2) {
            assert!(levels[pair[0]].is_subset(&levels[pair[1]]), "{pair:?}");
        }
        // Inherited, even for FCs.
        assert!(levels["admin"].contains("waitlist-tag:TRAINEE"));
        assert!(levels["fc"].contains("waitlist-tag:HQ-FC"));
        assert!(!levels["fc"].contains("commanders-manage"));
        assert!(levels["fc+"].contains("commanders-manage:fc"));
        assert!(!levels["fc+"].contains("commanders-manage:admin"));
    }

    #[test]
    fn unknown_role_has_no_keys() {
        assert!(access_keys("superuser").is_none());
        assert!(access_keys("user").is_some());
    }
}
