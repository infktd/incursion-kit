//! Server configuration.
//!
//! Loaded from a TOML file (`WAITLIST_CONFIG`, default `./config.toml`) and then overridden by
//! environment variables of the form `WAITLIST__<SECTION>__<KEY>` (e.g.
//! `WAITLIST__ESI__CLIENT_SECRET`). The file format is a superset of the legacy waitlist
//! `config.toml`, so an existing config file works unchanged; keys we no longer need (such as
//! `[sse]`, now served in-process) are accepted and ignored.

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not load configuration: {0}")]
    Load(#[from] config::ConfigError),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub app: AppConfig,
    pub esi: EsiConfig,
    #[serde(default)]
    pub data: DataConfig,
    #[serde(default)]
    pub fleet_updater: FleetUpdaterConfig,
    #[serde(default)]
    pub skill_updater: SkillUpdaterConfig,
    #[serde(default)]
    pub srp_updater: SrpUpdaterConfig,
    #[serde(default)]
    pub incursion_updater: IntervalUpdaterConfig,
    /// The public incursion tracker (`/api/incursions*`). While it runs it also maintains the
    /// high-sec focus (when `incursion_updater.enable`), replacing the legacy updater.
    #[serde(default)]
    pub incursion_tracker: IncursionTrackerConfig,
    #[serde(default)]
    pub war_updater: IntervalUpdaterConfig,
    #[serde(default)]
    pub janice: JaniceConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    /// SRP subscription tiers and coverage rules. The defaults are the legacy waitlist's.
    #[serde(default)]
    pub srp: SrpConfig,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ServerConfig {
    /// Address the HTTP server binds to.
    #[serde(default = "default_bind")]
    pub bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

fn default_bind() -> String {
    "0.0.0.0:5000".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct DatabaseConfig {
    pub url: String,
    #[serde(default = "default_min_connections")]
    pub min_connections: u32,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u64,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,
}

fn default_min_connections() -> u32 {
    1
}
fn default_max_connections() -> u32 {
    10
}
fn default_idle_timeout() -> u64 {
    60
}
fn default_connect_timeout() -> u64 {
    5
}

#[derive(Deserialize, Clone, Debug)]
pub struct AppConfig {
    /// 32-byte hex key used to encrypt session tokens. Keeping the same key as a previous
    /// install keeps existing sessions valid after migrating.
    pub token_secret: String,
}

impl AppConfig {
    pub fn token_key(&self) -> Result<[u8; 32], ConfigError> {
        decode_key(&self.token_secret, "app.token_secret")
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct EsiConfig {
    pub client_id: String,
    pub client_secret: String,
    /// The SSO callback URL registered with CCP: the frontend page `/auth/cb`.
    pub url: String,
    /// `User-Agent` sent to ESI and EVE SSO. CCP asks for an app name, a contact (email or
    /// in-game name) and a URL. Defaults to `incursion-kit/<version> (+<site origin>)`, derived
    /// from `url`.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// ESI base URL override, e.g. a local stand-in for testing. Defaults to CCP's ESI.
    #[serde(default)]
    pub esi_url: Option<String>,
    /// EVE SSO authorize URL override. Defaults to CCP's login server.
    #[serde(default)]
    pub sso_authorize_url: Option<String>,
    /// EVE SSO token URL override. Defaults to CCP's login server.
    #[serde(default)]
    pub sso_token_url: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DataConfig {
    /// Directory with the community data files (categories, fits, skills, guides, locales...).
    /// Admin endpoints write here, so it must be writable.
    #[serde(default = "default_data_dir")]
    pub dir: String,
    /// Path to the EVE static data file produced by `waitlist sde`.
    #[serde(default = "default_sde_path")]
    pub sde: String,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: default_data_dir(),
            sde: default_sde_path(),
        }
    }
}

fn default_data_dir() -> String {
    "./data".to_string()
}
fn default_sde_path() -> String {
    "./sde/sde.sqlite".to_string()
}

#[derive(Deserialize, Clone, Debug)]
pub struct FleetUpdaterConfig {
    #[serde(default = "yes")]
    pub enable: bool,
    #[serde(default = "default_min_in_fleet")]
    pub min_in_fleet: usize,
}

impl Default for FleetUpdaterConfig {
    fn default() -> Self {
        Self {
            enable: true,
            min_in_fleet: default_min_in_fleet(),
        }
    }
}

fn default_min_in_fleet() -> usize {
    8
}

#[derive(Deserialize, Clone, Debug)]
pub struct SkillUpdaterConfig {
    #[serde(default = "yes")]
    pub enable: bool,
    /// Seconds over which a full refresh of every pilot's skills is spread.
    #[serde(default = "default_skill_runtime")]
    pub runtime: f64,
}

impl Default for SkillUpdaterConfig {
    fn default() -> Self {
        Self {
            enable: true,
            runtime: default_skill_runtime(),
        }
    }
}

fn default_skill_runtime() -> f64 {
    86400.0
}

#[derive(Deserialize, Clone, Debug)]
pub struct SrpUpdaterConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_srp_interval")]
    pub interval_seconds: u64,
    /// Fraction of the normal SRP price that FCs may pay. 0.5 = 50%.
    #[serde(default = "default_fc_srp_price_factor")]
    pub fc_srp_price_factor: f64,
}

impl Default for SrpUpdaterConfig {
    fn default() -> Self {
        Self {
            enable: false,
            interval_seconds: default_srp_interval(),
            fc_srp_price_factor: default_fc_srp_price_factor(),
        }
    }
}

fn default_srp_interval() -> u64 {
    300
}
fn default_fc_srp_price_factor() -> f64 {
    0.5
}

#[derive(Deserialize, Clone, Debug)]
pub struct IntervalUpdaterConfig {
    #[serde(default = "yes")]
    pub enable: bool,
    #[serde(default = "default_hourly")]
    pub interval_seconds: u64,
}

impl Default for IntervalUpdaterConfig {
    fn default() -> Self {
        Self {
            enable: true,
            interval_seconds: default_hourly(),
        }
    }
}

fn default_hourly() -> u64 {
    3600
}

/// The `[incursion_tracker]` section.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct IncursionTrackerConfig {
    #[serde(default = "yes")]
    pub enable: bool,
    /// Seconds between polls when ESI sends no `Expires` header. Normally the tracker polls
    /// right after ESI's cache expires (every 5 minutes).
    #[serde(default = "default_tracker_interval")]
    pub interval_seconds: u64,
    /// Systems (names) the API reports jump counts from, e.g. trade hubs and the community's
    /// home or staging system.
    #[serde(default = "default_reference_systems")]
    pub reference_systems: Vec<String>,
    /// How many incursions of each security band the game runs at once; spawn windows are
    /// reported for empty slots.
    #[serde(default)]
    pub slots: IncursionSlotsConfig,
    #[serde(default)]
    pub discord: IncursionDiscordConfig,
}

impl Default for IncursionTrackerConfig {
    fn default() -> Self {
        Self {
            enable: true,
            interval_seconds: default_tracker_interval(),
            reference_systems: default_reference_systems(),
            slots: IncursionSlotsConfig::default(),
            discord: IncursionDiscordConfig::default(),
        }
    }
}

fn default_tracker_interval() -> u64 {
    300
}
fn default_reference_systems() -> Vec<String> {
    ["Jita", "Amarr", "Dodixie", "Rens", "Hek"]
        .map(String::from)
        .to_vec()
}

/// Concurrent incursions per security band (`[incursion_tracker.slots]`).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct IncursionSlotsConfig {
    #[serde(default = "one")]
    pub highsec: u32,
    #[serde(default = "one")]
    pub lowsec: u32,
    #[serde(default = "three")]
    pub nullsec: u32,
}

impl Default for IncursionSlotsConfig {
    fn default() -> Self {
        Self {
            highsec: 1,
            lowsec: 1,
            nullsec: 3,
        }
    }
}

fn one() -> u32 {
    1
}
fn three() -> u32 {
    3
}

/// Discord webhook notices for incursion changes (`[incursion_tracker.discord]`), off by
/// default.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct IncursionDiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook_url: String,
    /// Role pinged by new-spawn notices (empty: no ping).
    #[serde(default)]
    pub ping_role_id: String,
    /// Security bands to post about (`highsec`, `lowsec`, `nullsec`).
    #[serde(default = "default_discord_bands")]
    pub bands: Vec<String>,
    #[serde(default = "yes")]
    pub notify_spawn: bool,
    #[serde(default = "yes")]
    pub notify_state: bool,
    #[serde(default = "yes")]
    pub notify_boss: bool,
    #[serde(default = "yes")]
    pub notify_end: bool,
}

impl Default for IncursionDiscordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            webhook_url: String::new(),
            ping_role_id: String::new(),
            bands: default_discord_bands(),
            notify_spawn: true,
            notify_state: true,
            notify_boss: true,
            notify_end: true,
        }
    }
}

fn default_discord_bands() -> Vec<String> {
    ["highsec", "lowsec", "nullsec"].map(String::from).to_vec()
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct JaniceConfig {
    #[serde(default)]
    pub api_key: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub srp_webhook_url: String,
    #[serde(default)]
    pub site_url: String,
    #[serde(default)]
    pub srp_ping_role_id: String,
}

/// The `[srp]` section: what the ship replacement program sells and how long it covers.
///
/// Pilots buy coverage by sending ISK to the corporation wallet the SRP service account reads.
/// A deposit buys coverage when its amount (in millions of ISK) equals one of the prices below;
/// the n-th price of a list covers n characters. FC-eligible pilots may also pay a price times
/// `srp_updater.fc_srp_price_factor`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct SrpConfig {
    /// Daily coverage prices, in millions of ISK (entry n covers n characters).
    #[serde(default = "default_srp_daily_prices")]
    pub daily_prices: Vec<f64>,
    /// Per-focus coverage prices, in millions of ISK (entry n covers n characters).
    #[serde(default = "default_srp_per_focus_prices")]
    pub per_focus_prices: Vec<f64>,
    /// Daily coverage ends at this hour (UTC): the same day when paid before it, else the next.
    #[serde(default = "default_srp_daily_reset_hour")]
    pub daily_reset_hour_utc: u32,
    /// Per-focus coverage lasts this many days, or until the incursion focus ends.
    #[serde(default = "default_srp_per_focus_days")]
    pub per_focus_days: i64,
    /// Roles (of the paying character or its main) that may pay the FC price.
    #[serde(default = "default_srp_fc_roles")]
    pub fc_discount_roles: Vec<String>,
    /// How many days of wallet journal each reconciliation run rebuilds payments from.
    #[serde(default = "default_srp_journal_days")]
    pub journal_days: i64,
    /// An outgoing journal entry pays an approved report submitted at most this many days
    /// before it.
    #[serde(default = "default_srp_payout_match_days")]
    pub payout_match_days: i64,
}

impl Default for SrpConfig {
    fn default() -> Self {
        Self {
            daily_prices: default_srp_daily_prices(),
            per_focus_prices: default_srp_per_focus_prices(),
            daily_reset_hour_utc: default_srp_daily_reset_hour(),
            per_focus_days: default_srp_per_focus_days(),
            fc_discount_roles: default_srp_fc_roles(),
            journal_days: default_srp_journal_days(),
            payout_match_days: default_srp_payout_match_days(),
        }
    }
}

fn default_srp_daily_prices() -> Vec<f64> {
    vec![20.0, 35.0, 45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0, 80.0]
}
fn default_srp_per_focus_prices() -> Vec<f64> {
    vec![
        125.0, 225.0, 295.0, 330.0, 365.0, 400.0, 435.0, 470.0, 505.0, 540.0, 600.0, 680.0,
    ]
}
fn default_srp_daily_reset_hour() -> u32 {
    11
}
fn default_srp_per_focus_days() -> i64 {
    8
}
fn default_srp_fc_roles() -> Vec<String> {
    ["fc", "fc+", "admin"].map(String::from).to_vec()
}
fn default_srp_journal_days() -> i64 {
    8
}
fn default_srp_payout_match_days() -> i64 {
    7
}

fn yes() -> bool {
    true
}

impl Config {
    /// Load from `WAITLIST_CONFIG` (default `./config.toml`, optional) plus `WAITLIST__*` env vars.
    pub fn load() -> Result<Self, ConfigError> {
        let path = std::env::var("WAITLIST_CONFIG").unwrap_or_else(|_| "./config.toml".into());
        Self::load_from(&path)
    }

    pub fn load_from(path: &str) -> Result<Self, ConfigError> {
        let config: Config = config::Config::builder()
            .add_source(
                config::File::with_name(path)
                    .format(config::FileFormat::Toml)
                    .required(false),
            )
            .add_source(
                config::Environment::with_prefix("WAITLIST")
                    .prefix_separator("__")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?
            .try_deserialize()?;
        config.validate()?;
        Ok(config)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let config: Config = config::Config::builder()
            .add_source(config::File::from_str(raw, config::FileFormat::Toml))
            .build()?
            .try_deserialize()?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.app.token_key()?;
        if !(0.0..=1.0).contains(&self.srp_updater.fc_srp_price_factor) {
            return Err(ConfigError::Invalid(
                "srp_updater.fc_srp_price_factor must be between 0 and 1".into(),
            ));
        }
        let tracker = &self.incursion_tracker;
        if let Some(band) = tracker
            .discord
            .bands
            .iter()
            .find(|b| !["highsec", "lowsec", "nullsec"].contains(&b.as_str()))
        {
            return Err(ConfigError::Invalid(format!(
                "incursion_tracker.discord.bands: unknown band {band:?} \
                 (expected highsec, lowsec or nullsec)"
            )));
        }
        let srp = &self.srp;
        if srp.daily_reset_hour_utc > 23 {
            return Err(ConfigError::Invalid(
                "srp.daily_reset_hour_utc must be between 0 and 23".into(),
            ));
        }
        if srp.per_focus_days < 1 || srp.journal_days < 1 || srp.payout_match_days < 0 {
            return Err(ConfigError::Invalid(
                "srp.per_focus_days and srp.journal_days must be at least 1, \
                 srp.payout_match_days at least 0"
                    .into(),
            ));
        }
        if srp
            .daily_prices
            .iter()
            .chain(&srp.per_focus_prices)
            .any(|p| !p.is_finite() || *p <= 0.0)
        {
            return Err(ConfigError::Invalid(
                "srp prices must be positive numbers (millions of ISK)".into(),
            ));
        }
        Ok(())
    }

    /// True when the public site is served over HTTPS (derived from the SSO callback URL).
    pub fn is_https(&self) -> bool {
        self.esi.url.starts_with("https:")
    }
}

fn decode_key(hex_key: &str, name: &str) -> Result<[u8; 32], ConfigError> {
    let bytes = (0..hex_key.len())
        .step_by(2)
        .map(|i| {
            hex_key
                .get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()
        .filter(|b| b.len() == 32 && hex_key.len() == 64)
        .ok_or_else(|| ConfigError::Invalid(format!("{name} must be 64 hex characters")))?;
    Ok(bytes.try_into().expect("length checked"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"
[database]
url = "mysql://u:p@localhost/waitlist"
min_connections = 1
max_connections = 5
connect_timeout = 2
idle_timeout = 60

[app]
token_secret = "0000000000000000000000000000000000000000000000000000000000000000"

[esi]
client_id = "id"
client_secret = "secret"
url = "https://waitlist.example.com/auth/cb"

[sse]
url = "http://localhost:8000"
secret = "0000000000000000000000000000000000000000000000000000000000000000"

[fleet_updater]
enable = true
min_in_fleet = 8

[skill_updater]
enable = true
runtime = 86400

[srp_updater]
enable = true
interval_seconds = 300
fc_srp_price_factor = 0.5

[incursion_updater]
enable = true
interval_seconds = 3600

[war_updater]
enable = true
interval_seconds = 3600

[janice]
api_key = "k"

[discord]
enabled = false
srp_webhook_url = ""
site_url = ""
srp_ping_role_id = ""
"#;

    #[test]
    fn accepts_legacy_config_file() {
        let config = Config::from_toml_str(LEGACY).unwrap();
        assert_eq!(config.database.max_connections, 5);
        assert!(config.is_https());
        assert_eq!(config.server.bind, "0.0.0.0:5000");
        assert_eq!(config.data.dir, "./data");
    }

    #[test]
    fn incursion_tracker_defaults_and_overrides() {
        let config = Config::from_toml_str(LEGACY).unwrap();
        let tracker = &config.incursion_tracker;
        assert!(tracker.enable);
        assert_eq!(tracker.interval_seconds, 300);
        assert_eq!(tracker.reference_systems[0], "Jita");
        assert_eq!(tracker.slots.nullsec, 3);
        assert!(!tracker.discord.enabled);

        let custom = format!(
            "{LEGACY}\n[incursion_tracker]\nreference_systems = [\"Jita\", \"Hek\"]\n\
             [incursion_tracker.slots]\nnullsec = 2\n\
             [incursion_tracker.discord]\nenabled = true\nbands = [\"highsec\"]\n"
        );
        let tracker = Config::from_toml_str(&custom).unwrap().incursion_tracker;
        assert_eq!(tracker.reference_systems, ["Jita", "Hek"]);
        assert_eq!(tracker.slots.nullsec, 2);
        assert_eq!(tracker.slots.highsec, 1);
        assert_eq!(tracker.discord.bands, ["highsec"]);
        assert!(tracker.discord.notify_end);

        let bad = format!("{LEGACY}\n[incursion_tracker.discord]\nbands = [\"wormhole\"]\n");
        assert!(Config::from_toml_str(&bad).is_err());
    }

    #[test]
    fn rejects_bad_token_secret() {
        let raw = LEGACY.replace(
            "token_secret = \"0000000000000000000000000000000000000000000000000000000000000000\"",
            "token_secret = \"abc\"",
        );
        assert!(Config::from_toml_str(&raw).is_err());
    }

    #[test]
    fn srp_tiers_default_to_the_legacy_values_and_are_configurable() {
        let srp = Config::from_toml_str(LEGACY).unwrap().srp;
        assert_eq!(srp, SrpConfig::default());
        assert_eq!(srp.daily_prices.len(), 10);
        assert_eq!(srp.per_focus_prices[11], 680.0);
        assert_eq!(srp.daily_reset_hour_utc, 11);

        let custom = format!(
            "{LEGACY}\n[srp]\ndaily_prices = [15, 25.5]\nper_focus_prices = []\n\
             daily_reset_hour_utc = 0\nfc_discount_roles = [\"fc\"]\n"
        );
        let srp = Config::from_toml_str(&custom).unwrap().srp;
        assert_eq!(srp.daily_prices, [15.0, 25.5]);
        assert!(srp.per_focus_prices.is_empty());
        assert_eq!(srp.fc_discount_roles, ["fc"]);
        assert_eq!(srp.per_focus_days, 8);

        let bad = format!("{LEGACY}\n[srp]\ndaily_reset_hour_utc = 24\n");
        assert!(Config::from_toml_str(&bad).is_err());
        let bad = format!("{LEGACY}\n[srp]\ndaily_prices = [0]\n");
        assert!(Config::from_toml_str(&bad).is_err());
    }
}
