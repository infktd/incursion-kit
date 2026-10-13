//! ESI scopes the waitlist asks for, and the SSO login variants.

use std::collections::BTreeSet;
use std::fmt;

/// An ESI scope the waitlist uses (the legacy waitlist's scope table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    PublicData,
    FleetsReadFleet,
    FleetsWriteFleet,
    UiOpenWindow,
    SkillsReadSkills,
    ClonesReadImplants,
    SearchStructures,
    WalletReadCorporationWallets,
}

impl Scope {
    /// The scope as EVE SSO names it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::PublicData => "publicData",
            Scope::FleetsReadFleet => "esi-fleets.read_fleet.v1",
            Scope::FleetsWriteFleet => "esi-fleets.write_fleet.v1",
            Scope::UiOpenWindow => "esi-ui.open_window.v1",
            Scope::SkillsReadSkills => "esi-skills.read_skills.v1",
            Scope::ClonesReadImplants => "esi-clones.read_implants.v1",
            Scope::SearchStructures => "esi-search.search_structures.v1",
            Scope::WalletReadCorporationWallets => "esi-wallet.read_corporation_wallets.v1",
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an SSO login is for. Sent to EVE SSO as the OAuth `state` and handed back to the
/// callback unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginKind {
    /// Log in (or switch account) as the character.
    Normal,
    /// Link the character as an alt of the logged-in account.
    Alt,
    /// Re-authenticate a character whose token the SRP admin pages use to open windows.
    SrpAdmin,
    /// Set the character up as the SRP service account (corporation wallet access).
    SrpSetup,
}

impl LoginKind {
    /// The `state` value the legacy frontend and callback expect.
    pub fn state(&self) -> &'static str {
        match self {
            LoginKind::Normal => "normal",
            LoginKind::Alt => "alt",
            LoginKind::SrpAdmin => "srp_admin",
            LoginKind::SrpSetup => "srp_setup",
        }
    }

    /// Parse a callback's `state`. A suffix after `:` (e.g. a CSRF nonce the API appends:
    /// `alt:3f9c...`) is ignored here; checking it is the caller's job. Unknown values are
    /// `None`; the legacy callback treated them (and a missing state) as [`LoginKind::Normal`].
    pub fn from_state(state: &str) -> Option<Self> {
        match state.split(':').next().unwrap_or_default() {
            "normal" => Some(LoginKind::Normal),
            "alt" => Some(LoginKind::Alt),
            "srp_admin" => Some(LoginKind::SrpAdmin),
            "srp_setup" => Some(LoginKind::SrpSetup),
            _ => None,
        }
    }

    /// The scopes requested for this login, as the legacy `login_url` built them: every login
    /// asks for public data, skills and implants; `fc` adds fleet read/write, open-window and
    /// search; an SRP admin login adds open-window. The SRP service account asks for public data
    /// and corporation wallets only.
    pub fn scopes(&self, fc: bool) -> Vec<Scope> {
        if *self == LoginKind::SrpSetup {
            return vec![Scope::PublicData, Scope::WalletReadCorporationWallets];
        }
        let mut scopes = vec![
            Scope::PublicData,
            Scope::SkillsReadSkills,
            Scope::ClonesReadImplants,
        ];
        if fc {
            scopes.extend([
                Scope::FleetsReadFleet,
                Scope::FleetsWriteFleet,
                Scope::UiOpenWindow,
                Scope::SearchStructures,
            ]);
        }
        if *self == LoginKind::SrpAdmin && !scopes.contains(&Scope::UiOpenWindow) {
            scopes.push(Scope::UiOpenWindow);
        }
        scopes
    }
}

/// Parse the space-separated scopes stored in the `scopes` columns.
pub fn split_scopes(input: &str) -> BTreeSet<String> {
    input
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Format scopes for the `scopes` columns exactly as the legacy waitlist wrote them: each scope
/// followed by a space.
pub fn join_scopes(scopes: &BTreeSet<String>) -> String {
    scopes.iter().fold(String::new(), |acc, s| acc + s + " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(scopes: Vec<Scope>) -> Vec<&'static str> {
        scopes.iter().map(Scope::as_str).collect()
    }

    #[test]
    fn login_scope_sets_match_the_legacy_login_url() {
        assert_eq!(
            names(LoginKind::Normal.scopes(false)),
            [
                "publicData",
                "esi-skills.read_skills.v1",
                "esi-clones.read_implants.v1"
            ]
        );
        assert_eq!(
            names(LoginKind::Alt.scopes(false)),
            names(LoginKind::Normal.scopes(false))
        );
        assert_eq!(
            names(LoginKind::Normal.scopes(true)),
            [
                "publicData",
                "esi-skills.read_skills.v1",
                "esi-clones.read_implants.v1",
                "esi-fleets.read_fleet.v1",
                "esi-fleets.write_fleet.v1",
                "esi-ui.open_window.v1",
                "esi-search.search_structures.v1",
            ]
        );
        assert_eq!(
            names(LoginKind::SrpAdmin.scopes(false)),
            [
                "publicData",
                "esi-skills.read_skills.v1",
                "esi-clones.read_implants.v1",
                "esi-ui.open_window.v1",
            ]
        );
        assert_eq!(
            names(LoginKind::SrpSetup.scopes(true)),
            ["publicData", "esi-wallet.read_corporation_wallets.v1"]
        );
    }

    #[test]
    fn states_round_trip_and_ignore_a_nonce() {
        for kind in [
            LoginKind::Normal,
            LoginKind::Alt,
            LoginKind::SrpAdmin,
            LoginKind::SrpSetup,
        ] {
            assert_eq!(LoginKind::from_state(kind.state()), Some(kind));
        }
        assert_eq!(LoginKind::from_state("alt:abc123"), Some(LoginKind::Alt));
        assert_eq!(LoginKind::from_state("evil"), None);
        assert_eq!(LoginKind::from_state(""), None);
    }

    #[test]
    fn scope_columns_keep_the_legacy_format() {
        let scopes = split_scopes("publicData  esi-skills.read_skills.v1 ");
        assert_eq!(scopes.len(), 2);
        assert_eq!(
            join_scopes(&scopes),
            "esi-skills.read_skills.v1 publicData "
        );
        assert!(split_scopes("").is_empty());
    }
}
