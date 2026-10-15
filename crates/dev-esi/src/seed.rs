//! The seed: the world dev-esi starts with (and returns to on reset).
//!
//! A seed is a YAML file (or TOML, by extension); [`DEFAULT_SEED`] is the one shipped with the
//! crate (`crates/dev-esi/seed.yaml`). Times may be written relative to when the seed is loaded
//! (`now`, `-3h`, `-2d`, `+30m`) as well as in RFC 3339, so a seed's killmails, wallet journal and
//! wars stay recent. Unknown keys are errors, so a typo doesn't silently drop data.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The seed shipped with the crate.
pub const DEFAULT_SEED: &str = include_str!("../seed.yaml");

/// Why a seed could not be used.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("could not read the seed {path}: {error}")]
    Read { path: String, error: std::io::Error },
    #[error("invalid seed: {0}")]
    Parse(String),
    #[error("inconsistent seed: {0}")]
    Invalid(String),
}

/// Where a seed comes from. Kept so a reset re-reads it (relative times move with the reset).
#[derive(Debug, Clone)]
pub struct SeedSource {
    text: String,
    toml: bool,
}

impl SeedSource {
    /// The seed shipped with the crate.
    pub fn default_seed() -> Self {
        Self::yaml(DEFAULT_SEED)
    }

    pub fn yaml(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            toml: false,
        }
    }

    pub fn toml(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            toml: true,
        }
    }

    /// A seed file: TOML when it ends in `.toml`, YAML otherwise.
    pub fn from_path(path: &Path) -> Result<Self, SeedError> {
        let text = std::fs::read_to_string(path).map_err(|error| SeedError::Read {
            path: path.display().to_string(),
            error,
        })?;
        let toml = path.extension().is_some_and(|e| e == "toml");
        Ok(Self { text, toml })
    }

    /// Parse (resolving relative times against now) and check the seed.
    pub fn parse(&self) -> Result<Seed, SeedError> {
        let seed: Seed = if self.toml {
            toml::from_str(&self.text).map_err(|e| SeedError::Parse(e.to_string()))?
        } else {
            let options = serde_saphyr::options! {
                merge_keys: serde_saphyr::MergeKeyPolicy::Merge,
            };
            serde_saphyr::from_str_with_options(&self.text, options)
                .map_err(|e| SeedError::Parse(e.to_string()))?
        };
        seed.validate()?;
        Ok(seed)
    }
}

/// A point in time: RFC 3339, `now`, or an offset from now such as `-3h`, `-2d`, `+30m`, `-90s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SeedTime(pub DateTime<Utc>);

impl SeedTime {
    pub fn now() -> Self {
        // ESI timestamps have whole seconds.
        let now = Utc::now();
        Self(DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now))
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::parse_at(value, Self::now())
    }

    /// [`SeedTime::parse`] with relative times relative to `now`.
    pub fn parse_at(value: &str, now: Self) -> Option<Self> {
        let value = value.trim();
        if value == "now" {
            return Some(now);
        }
        if let Ok(time) = DateTime::parse_from_rfc3339(value) {
            return Some(Self(time.with_timezone(&Utc)));
        }
        let (sign, rest) = match value.as_bytes().first()? {
            b'-' => (-1, &value[1..]),
            b'+' => (1, &value[1..]),
            _ => return None,
        };
        let unit = rest.chars().last()?;
        let amount: i64 = rest[..rest.len() - unit.len_utf8()].parse().ok()?;
        let offset = match unit {
            's' => Duration::seconds(amount),
            'm' => Duration::minutes(amount),
            'h' => Duration::hours(amount),
            'd' => Duration::days(amount),
            _ => return None,
        };
        Some(Self(now.0 + offset * sign))
    }

    /// As ESI formats times: `2026-10-14T12:00:00Z`.
    pub fn esi(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Secs, true)
    }
}

impl Serialize for SeedTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.esi())
    }
}

impl<'de> Deserialize<'de> for SeedTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid time {value:?} (expected RFC 3339, `now` or an offset like `-3h`)"
            ))
        })
    }
}

/// The whole seed file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seed {
    #[serde(default)]
    pub sso: SsoSettings,
    #[serde(default)]
    pub fleet: FleetSettings,
    #[serde(default)]
    pub esi: EsiSettings,
    /// Named skill sets (`type id: level`) characters can start from.
    #[serde(default)]
    pub skill_sets: BTreeMap<String, BTreeMap<i64, i64>>,
    #[serde(default)]
    pub alliances: Vec<Alliance>,
    #[serde(default)]
    pub corporations: Vec<Corporation>,
    #[serde(default)]
    pub characters: Vec<Character>,
    /// Fleets that exist at start.
    #[serde(default)]
    pub fleets: Vec<FleetSeed>,
    #[serde(default)]
    pub killmails: Vec<Killmail>,
    #[serde(default)]
    pub wars: Vec<War>,
    #[serde(default)]
    pub incursions: Vec<Incursion>,
    /// More names for `POST /universe/names` and `GET /universe/types/{id}` (factions, or types
    /// when no static data file is loaded).
    #[serde(default)]
    pub names: Vec<NameEntry>,
}

/// EVE SSO behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SsoSettings {
    /// Registered applications. Empty means one application, `dev-esi-client` with secret
    /// `dev-esi-secret`, accepting any callback URL.
    #[serde(default)]
    pub clients: Vec<SsoClient>,
    /// Access token lifetime (EVE: 1199 s).
    #[serde(default = "default_token_lifetime")]
    pub access_token_lifetime_secs: u64,
    /// The `iss` claim. EVE's own value by default, so a verifier configured with dev-esi's key
    /// set accepts the tokens.
    #[serde(default = "default_issuer")]
    pub issuer: String,
}

impl Default for SsoSettings {
    fn default() -> Self {
        Self {
            clients: Vec::new(),
            access_token_lifetime_secs: default_token_lifetime(),
            issuer: default_issuer(),
        }
    }
}

fn default_token_lifetime() -> u64 {
    1199
}
fn default_issuer() -> String {
    "https://login.eveonline.com".to_string()
}

/// A registered SSO application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SsoClient {
    pub client_id: String,
    /// `None`: a public application (PKCE only).
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Allowed callback URLs; empty allows any.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
}

impl SsoClient {
    /// The application used when a seed registers none.
    pub fn default_client() -> Self {
        Self {
            client_id: "dev-esi-client".to_string(),
            client_secret: Some("dev-esi-secret".to_string()),
            redirect_uris: Vec::new(),
        }
    }
}

/// Fleet simulation settings (also changeable at runtime through `PUT /_dev/settings`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetSettings {
    /// How long a pilot takes to accept an invite (pilots with `auto_accept_invites`).
    #[serde(default = "default_accept_delay")]
    pub invite_accept_delay_ms: u64,
    /// Wings and squads of a new fleet.
    #[serde(default = "default_layout")]
    pub default_layout: Vec<WingLayout>,
}

impl Default for FleetSettings {
    fn default() -> Self {
        Self {
            invite_accept_delay_ms: default_accept_delay(),
            default_layout: default_layout(),
        }
    }
}

fn default_accept_delay() -> u64 {
    2000
}

fn default_layout() -> Vec<WingLayout> {
    vec![
        WingLayout {
            name: "On Grid".to_string(),
            squads: ["Logi", "Bastion", "Other", "Alts"]
                .map(String::from)
                .to_vec(),
        },
        WingLayout {
            name: "Reserve".to_string(),
            squads: vec!["Reserve".to_string()],
        },
    ]
}

/// A wing and its squads' names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WingLayout {
    pub name: String,
    pub squads: Vec<String>,
}

/// ESI response caching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EsiSettings {
    /// `Expires` of `GET /incursions` (EVE: 300 s). Clients poll again after it, so an incursion
    /// change made through the control API shows up at most this late.
    #[serde(default = "default_incursions_cache")]
    pub incursions_cache_secs: u64,
}

impl Default for EsiSettings {
    fn default() -> Self {
        Self {
            incursions_cache_secs: default_incursions_cache(),
        }
    }
}

fn default_incursions_cache() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Alliance {
    pub id: i64,
    pub name: String,
    pub ticker: String,
    #[serde(default)]
    pub executor_corporation_id: Option<i64>,
    #[serde(default = "long_ago")]
    pub date_founded: SeedTime,
}

fn long_ago() -> SeedTime {
    SeedTime(DateTime::from_timestamp(1_577_836_800, 0).unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Corporation {
    pub id: i64,
    pub name: String,
    pub ticker: String,
    #[serde(default)]
    pub alliance_id: Option<i64>,
    #[serde(default)]
    pub ceo_id: Option<i64>,
    #[serde(default)]
    pub description: String,
    /// Wallet divisions (1-7); divisions not listed have a zero balance.
    #[serde(default)]
    pub wallets: Vec<Wallet>,
    /// Wallet journal entries (in any order; served newest first).
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
    /// Not used by ESI: `dev-esi sql` bans the corporation on the waitlist with this public
    /// reason.
    #[serde(default)]
    pub waitlist_ban: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Wallet {
    pub division: i64,
    pub balance: f64,
}

fn master_wallet() -> i64 {
    1
}

/// A wallet journal entry as ESI sends it, plus the wallet division it is in (not sent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    /// 1 (the master wallet) to 7.
    #[serde(default = "master_wallet")]
    pub division: i64,
    /// 0 in a seed or control request: assigned.
    #[serde(default)]
    pub id: i64,
    #[serde(default = "SeedTime::now")]
    pub date: SeedTime,
    #[serde(default = "player_donation")]
    pub ref_type: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_party_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_party_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_receiver_id: Option<i64>,
}

fn player_donation() -> String {
    "player_donation".to_string()
}

/// A character. Most fields are what ESI reports; the rest drive the simulation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Character {
    pub id: i64,
    pub name: String,
    pub corporation_id: i64,
    /// Shown on the login page.
    #[serde(default)]
    pub notes: String,
    /// The owner hash (`owner` claim); derived from the ID when absent. Change it to simulate a
    /// character moving to another account.
    #[serde(default)]
    pub owner: Option<String>,
    /// A `skill_sets` entry the character starts from.
    #[serde(default)]
    pub skill_set: Option<String>,
    /// Skill levels on top of the skill set (`type id: level`; 0 removes a skill).
    #[serde(default)]
    pub skills: BTreeMap<i64, i64>,
    /// Plugged-in implants (type IDs).
    #[serde(default)]
    pub implants: Vec<i64>,
    /// The ship flown (a capsule by default).
    #[serde(default = "capsule")]
    pub ship_type_id: i64,
    #[serde(default = "jita")]
    pub solar_system_id: i64,
    #[serde(default)]
    pub station_id: Option<i64>,
    /// Accept fleet invites by itself after `fleet.invite_accept_delay_ms`.
    #[serde(default = "yes")]
    pub auto_accept_invites: bool,
    #[serde(default)]
    pub security_status: f64,
    #[serde(default = "long_ago")]
    pub birthday: SeedTime,
    /// Not used by ESI: `dev-esi sql` grants this waitlist role (`fc`, `admin`, ...).
    #[serde(default)]
    pub waitlist_role: Option<String>,
}

fn capsule() -> i64 {
    670
}
fn jita() -> i64 {
    30000142
}
fn yes() -> bool {
    true
}

/// A fleet that exists when the seed loads, with the default layout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetSeed {
    pub boss_id: i64,
    #[serde(default)]
    pub motd: String,
    /// Members besides the boss, each in the named squad (the first squad by default).
    #[serde(default)]
    pub members: Vec<FleetMemberSeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetMemberSeed {
    pub character_id: i64,
    #[serde(default)]
    pub squad: Option<String>,
}

/// A killmail as ESI sends it, plus its hash (not sent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Killmail {
    /// 40 hex characters; derived from the ID when absent.
    #[serde(default)]
    pub killmail_hash: String,
    /// 0 in a control request: assigned.
    #[serde(default)]
    pub killmail_id: i64,
    #[serde(default = "SeedTime::now")]
    pub killmail_time: SeedTime,
    pub solar_system_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moon_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub war_id: Option<i64>,
    pub victim: Victim,
    #[serde(default)]
    pub attackers: Vec<Attacker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Victim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alliance_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faction_id: Option<i64>,
    #[serde(default)]
    pub damage_taken: i64,
    pub ship_type_id: i64,
    #[serde(default)]
    pub items: Vec<KillmailItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KillmailItem {
    pub item_type_id: i64,
    pub flag: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_destroyed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_dropped: Option<i64>,
    #[serde(default)]
    pub singleton: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<KillmailItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attacker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alliance_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub faction_id: Option<i64>,
    #[serde(default)]
    pub damage_done: i64,
    #[serde(default)]
    pub final_blow: bool,
    #[serde(default)]
    pub security_status: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_type_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon_type_id: Option<i64>,
}

/// A war as ESI sends it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct War {
    pub id: i64,
    pub aggressor: WarSide,
    pub defender: WarSide,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allies: Vec<WarAlly>,
    #[serde(default = "SeedTime::now")]
    pub declared: SeedTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<SeedTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<SeedTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retracted: Option<SeedTime>,
    #[serde(default)]
    pub mutual: bool,
    #[serde(default)]
    pub open_for_allies: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarSide {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alliance_id: Option<i64>,
    #[serde(default)]
    pub isk_destroyed: f64,
    #[serde(default)]
    pub ships_killed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarAlly {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corporation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alliance_id: Option<i64>,
}

/// An incursion as ESI sends it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Incursion {
    pub constellation_id: i64,
    /// Sansha's Nation.
    #[serde(default = "sansha")]
    pub faction_id: i64,
    #[serde(default)]
    pub has_boss: bool,
    pub infested_solar_systems: Vec<i64>,
    #[serde(default)]
    pub influence: f64,
    pub staging_solar_system_id: i64,
    /// `mobilizing`, `established` or `withdrawing`.
    pub state: String,
    #[serde(rename = "type", default = "incursion_type")]
    pub type_: String,
}

fn sansha() -> i64 {
    500019
}
fn incursion_type() -> String {
    "Incursion".to_string()
}

/// A name `POST /universe/names` resolves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NameEntry {
    pub id: i64,
    pub name: String,
    /// `faction`, `inventory_type`, `solar_system`, ...
    pub category: String,
}

impl Seed {
    /// The registered SSO applications (the default one when none are).
    pub fn sso_clients(&self) -> Vec<SsoClient> {
        if self.sso.clients.is_empty() {
            vec![SsoClient::default_client()]
        } else {
            self.sso.clients.clone()
        }
    }

    fn validate(&self) -> Result<(), SeedError> {
        let invalid = |msg: String| Err(SeedError::Invalid(msg));
        let mut ids = std::collections::BTreeSet::new();
        let alliances: Vec<i64> = self.alliances.iter().map(|a| a.id).collect();
        let corporations: Vec<i64> = self.corporations.iter().map(|c| c.id).collect();
        for id in alliances
            .iter()
            .chain(&corporations)
            .chain(self.characters.iter().map(|c| &c.id))
        {
            if !ids.insert(*id) {
                return invalid(format!("ID {id} is used twice"));
            }
        }
        for corporation in &self.corporations {
            if let Some(alliance) = corporation.alliance_id
                && !alliances.contains(&alliance)
            {
                return invalid(format!(
                    "corporation {} names unknown alliance {alliance}",
                    corporation.id
                ));
            }
            if let Some(wallet) = corporation
                .wallets
                .iter()
                .map(|w| w.division)
                .chain(corporation.journal.iter().map(|e| e.division))
                .find(|d| !(1..=7).contains(d))
            {
                return invalid(format!(
                    "corporation {}: wallet division {wallet} is not 1-7",
                    corporation.id
                ));
            }
        }
        for character in &self.characters {
            if !corporations.contains(&character.corporation_id) {
                return invalid(format!(
                    "character {} names unknown corporation {}",
                    character.id, character.corporation_id
                ));
            }
            if let Some(set) = &character.skill_set
                && !self.skill_sets.contains_key(set)
            {
                return invalid(format!(
                    "character {} names unknown skill set {set:?}",
                    character.id
                ));
            }
        }
        let characters: Vec<i64> = self.characters.iter().map(|c| c.id).collect();
        for fleet in &self.fleets {
            for id in
                std::iter::once(fleet.boss_id).chain(fleet.members.iter().map(|m| m.character_id))
            {
                if !characters.contains(&id) {
                    return invalid(format!("fleet member {id} is not a seeded character"));
                }
            }
        }
        if self.sso_clients().iter().any(|c| c.client_id.is_empty()) {
            return invalid("an SSO client has an empty client_id".into());
        }
        Ok(())
    }
}

/// SQL that sets the waitlist up for this seed (roles from `waitlist_role`, corporation bans from
/// `waitlist_ban`). Safe to run again: roles are set again and existing bans are kept.
///
/// Characters are created as bare rows (ID and name) so roles can be granted before anyone logs
/// in; the waitlist fills in the corporation at the first login. Bans are issued by the first
/// `admin` (else the first character with any role).
pub fn waitlist_sql(seed: &Seed) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"));
    let mut sql = String::from(
        "-- Written by `dev-esi sql`: waitlist roles and bans for the dev-esi seed. Idempotent.\n",
    );
    let with_role: Vec<&Character> = seed
        .characters
        .iter()
        .filter(|c| c.waitlist_role.is_some())
        .collect();
    let banned: Vec<&Corporation> = seed
        .corporations
        .iter()
        .filter(|c| c.waitlist_ban.is_some())
        .collect();
    let issuer = with_role
        .iter()
        .find(|c| c.waitlist_role.as_deref() == Some("admin"))
        .or_else(|| with_role.first());
    for character in &with_role {
        let role = character.waitlist_role.as_deref().unwrap_or_default();
        sql.push_str(&format!(
            "INSERT INTO `character` (id, name) VALUES ({}, {}) ON DUPLICATE KEY UPDATE id = id;\n\
             INSERT INTO admin (character_id, role, granted_at, granted_by_id) \
             VALUES ({}, {}, UNIX_TIMESTAMP(), {}) ON DUPLICATE KEY UPDATE role = VALUES(role);\n",
            character.id,
            quote(&character.name),
            character.id,
            quote(role),
            character.id,
        ));
    }
    if let Some(issuer) = issuer {
        for corporation in banned {
            sql.push_str(&format!(
                "INSERT INTO ban (entity_id, entity_name, entity_type, issued_at, issued_by, \
                 public_reason, reason) SELECT {id}, {name}, 'Corporation', UNIX_TIMESTAMP(), {by}, \
                 {public}, 'dev-esi seed' FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM ban \
                 WHERE entity_id = {id} AND entity_type = 'Corporation' AND revoked_at IS NULL);\n",
                id = corporation.id,
                name = quote(&corporation.name),
                by = issuer.id,
                public = quote(corporation.waitlist_ban.as_deref().unwrap_or_default()),
            ));
        }
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_seed_loads() {
        let seed = SeedSource::default_seed().parse().unwrap();
        assert!(seed.characters.len() >= 10, "{}", seed.characters.len());
        assert!(seed.corporations.len() >= 3);
        assert!(!seed.incursions.is_empty());
        assert!(!seed.killmails.is_empty());
        assert!(seed.characters.iter().any(|c| c.waitlist_role.is_some()));
        assert!(seed.corporations.iter().any(|c| c.waitlist_ban.is_some()));
    }

    #[test]
    fn relative_times() {
        let now = SeedTime::now().0;
        let earlier = SeedTime::parse("-2h").unwrap().0;
        assert_eq!((now - earlier).num_hours(), 2);
        assert!(SeedTime::parse("+30m").unwrap().0 > now);
        assert_eq!(
            SeedTime::parse("2024-05-01T12:00:00Z").unwrap().esi(),
            "2024-05-01T12:00:00Z"
        );
        for bad in ["", "yesterday", "-3w", "-h", "2024-05-01"] {
            assert_eq!(SeedTime::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn inconsistent_seeds_are_refused() {
        let bad = "corporations: [{id: 1, name: A, ticker: A, alliance_id: 9}]";
        assert!(matches!(
            SeedSource::yaml(bad).parse(),
            Err(SeedError::Invalid(_))
        ));
        let typo = "characterz: []";
        assert!(matches!(
            SeedSource::yaml(typo).parse(),
            Err(SeedError::Parse(_))
        ));
        let toml = "[[corporations]]\nid = 1\nname = \"A\"\nticker = \"A\"\n\
                    [[characters]]\nid = 2\nname = \"B\"\ncorporation_id = 1\n";
        let seed = SeedSource::toml(toml).parse().unwrap();
        assert_eq!(seed.characters[0].ship_type_id, 670);
    }

    #[test]
    fn sql_grants_roles_and_bans_corporations() {
        let seed = SeedSource::default_seed().parse().unwrap();
        let sql = waitlist_sql(&seed);
        assert!(sql.contains("INSERT INTO admin"), "{sql}");
        assert!(sql.contains("'admin'"), "{sql}");
        assert!(sql.contains("INSERT INTO ban"), "{sql}");
        assert!(waitlist_sql(&Seed::default()).lines().count() == 1);
    }
}
