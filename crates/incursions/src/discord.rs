//! Optional Discord webhook notices (`[incursion_tracker.discord]`, off by default): a new
//! spawn, a state change, the boss spawning and the end of an incursion, for the configured
//! security bands. A failed post is logged and dropped.

use serde_json::{Value, json};
use wl_core::config::IncursionDiscordConfig;

use crate::store::kinds;
use crate::sync::Change;

/// Posts notices for tracker changes.
#[derive(Debug, Clone)]
pub struct Notifier {
    config: IncursionDiscordConfig,
    http: reqwest::Client,
}

impl Notifier {
    /// `None` unless notices are enabled and a webhook URL is set.
    pub fn new(config: &IncursionDiscordConfig) -> Option<Self> {
        (config.enabled && !config.webhook_url.trim().is_empty()).then(|| Self {
            config: config.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        })
    }

    /// The webhook message for a change, or `None` when it isn't configured to be posted.
    pub fn message(&self, change: &Change) -> Option<Value> {
        let config = &self.config;
        let spawn = &change.spawn;
        if !config.bands.contains(&spawn.security_band) {
            return None;
        }
        let place = format!(
            "{} ({}{})",
            spawn.constellation_name,
            spawn.region_name.as_deref().unwrap_or("unknown region"),
            match spawn.security_band.as_str() {
                "highsec" => ", high-sec",
                "lowsec" => ", low-sec",
                _ => ", null-sec",
            }
        );
        let (title, color, ping) = match change.kind {
            kinds::SPAWNED if config.notify_spawn => {
                (format!("New incursion: {place}"), 0x57F287, true)
            }
            kinds::STATE_CHANGED if config.notify_state => (
                format!(
                    "{place}: {} → {}",
                    change.from_state.as_deref().unwrap_or("?"),
                    change.to_state.as_deref().unwrap_or("?")
                ),
                0xFEE75C,
                false,
            ),
            kinds::BOSS_SPAWNED if config.notify_boss => {
                (format!("Boss spawned: {place}"), 0xED4245, false)
            }
            kinds::ENDED if config.notify_end => {
                (format!("Incursion ended: {place}"), 0x99AAB5, false)
            }
            _ => return None,
        };
        let role = config.ping_role_id.trim();
        let ping = ping && !role.is_empty();
        Some(json!({
            "content": ping.then(|| format!("<@&{role}>")),
            "allowed_mentions": {"parse": [], "roles": if ping { vec![role.to_string()] } else { vec![] }},
            "embeds": [{
                "title": title,
                "color": color,
                "fields": [
                    {"name": "Staging", "value": format!("{} ({:.1})", spawn.staging_system_name,
                        wl_eve_data::universe::display_security(spawn.staging_security)), "inline": true},
                    {"name": "State", "value": if spawn.is_active() { spawn.state.as_str() } else { "ended" }, "inline": true},
                    {"name": "Influence", "value": format!("{:.0} %", spawn.influence * 100.0), "inline": true},
                ],
            }],
        }))
    }

    /// Posts the notices for `changes`, in order.
    pub async fn notify(&self, changes: &[Change]) {
        for change in changes {
            let Some(message) = self.message(change) else {
                continue;
            };
            let result = self
                .http
                .post(&self.config.webhook_url)
                .json(&message)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status);
            if let Err(e) = result {
                tracing::warn!(error = %e, kind = change.kind, spawn_id = change.spawn.id,
                    "could not post the incursion notice to Discord");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SpawnRow;

    fn change(kind: &'static str, band: &str) -> Change {
        Change {
            kind,
            from_state: Some("established".into()),
            to_state: Some("mobilizing".into()),
            spawn: SpawnRow {
                id: 1,
                constellation_id: 20000396,
                constellation_name: "Vale".into(),
                region_id: Some(10000032),
                region_name: Some("Sinq Laison".into()),
                faction_id: 500019,
                incursion_type: "Incursion".into(),
                staging_system_id: 30002708,
                staging_system_name: "Claysson".into(),
                staging_security: 0.9377,
                security_band: band.into(),
                state: "mobilizing".into(),
                influence: 0.5,
                has_boss: 0,
                first_seen_at: 0,
                observed_from_start: 1,
                last_seen_at: 0,
                state_changed_at: 0,
                mobilizing_at: None,
                withdrawing_at: None,
                boss_spawned_at: None,
                ended_at: None,
            },
        }
    }

    #[test]
    fn messages_follow_the_config() {
        let mut config = IncursionDiscordConfig::default();
        assert!(Notifier::new(&config).is_none(), "off by default");
        config.enabled = true;
        config.webhook_url = "https://discord.test/hook".into();
        config.ping_role_id = "42".into();
        config.bands = vec!["highsec".into()];
        config.notify_state = false;
        let notifier = Notifier::new(&config).unwrap();

        let spawned = notifier
            .message(&change(kinds::SPAWNED, "highsec"))
            .unwrap();
        assert_eq!(spawned["content"], "<@&42>");
        assert_eq!(spawned["allowed_mentions"]["roles"][0], "42");
        assert_eq!(
            spawned["embeds"][0]["title"],
            "New incursion: Vale (Sinq Laison, high-sec)"
        );
        assert_eq!(spawned["embeds"][0]["fields"][0]["value"], "Claysson (0.9)");

        assert!(
            notifier
                .message(&change(kinds::SPAWNED, "nullsec"))
                .is_none()
        );
        assert!(
            notifier
                .message(&change(kinds::STATE_CHANGED, "highsec"))
                .is_none()
        );
        let ended = notifier.message(&change(kinds::ENDED, "highsec")).unwrap();
        assert!(ended["content"].is_null(), "only new spawns ping");
    }
}
