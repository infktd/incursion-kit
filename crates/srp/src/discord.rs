//! Discord webhook notices for SRP reports.
//!
//! When `discord.enabled` and `discord.srp_webhook_url` are set, a report's first notice (on
//! submission) is posted as a webhook message that pings `discord.srp_ping_role_id`; its id is
//! stored in `srp_reports.discord_message_id`, and later status changes (approve, deny, paid)
//! edit that message. Each embed links to `{site_url}/srp-report-detail?id=<killmail_id>`.

use serde::{Deserialize, Serialize};

use crate::Srp;

#[derive(Debug, thiserror::Error)]
pub enum DiscordError {
    #[error("HTTP error while contacting Discord: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Discord API error: {0}")]
    Api(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WebhookPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeds: Option<Vec<Embed>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mentions: Option<AllowedMentions>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AllowedMentions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Embed {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub color: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<EmbedField>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

#[derive(Deserialize)]
struct MessageResponse {
    id: String,
}

/// The report fields a notice shows.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NoticeReport {
    pub killmail_id: i64,
    pub status: String,
    pub victim_character_name: Option<String>,
    pub victim_ship_type: Option<String>,
    pub submitted_by_name: Option<String>,
    pub reason: Option<String>,
    pub discord_message_id: Option<i64>,
}

/// The site URL for report links: `discord.site_url`, else `esi.url` without its last path
/// segment, as the legacy waitlist derived it (`https://site/auth/cb` gives `https://site/auth`,
/// so set `discord.site_url`). No trailing slash; `http://` is added when there is no scheme.
pub fn site_url(configured: &str, esi_url: &str) -> String {
    if !configured.trim().is_empty() {
        return normalize_site_url(configured);
    }
    let derived = esi_url
        .rsplit_once('/')
        .map_or(esi_url, |(base, _)| base)
        .to_string();
    normalize_site_url(&derived)
}

fn normalize_site_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "approved" => "Approved",
        "rejected" => "Rejected",
        "paid" => "Paid",
        _ => "Pending",
    }
}

fn status_color(status: &str) -> u32 {
    match status {
        "approved" => 0x57F287,
        "rejected" => 0xED4245,
        "paid" => 0x5865F2,
        _ => 0xFEE75C,
    }
}

fn field(name: &str, value: &str) -> EmbedField {
    EmbedField {
        name: name.to_string(),
        value: value.to_string(),
        inline: Some(true),
    }
}

/// The webhook message for a report. `ping_role` adds the role mention (first notice only).
pub fn build_payload(
    report: &NoticeReport,
    site_url: &str,
    ping_role: bool,
    role_id: &str,
) -> WebhookPayload {
    let label = status_label(&report.status);
    let unknown = |v: &Option<String>| v.clone().unwrap_or_else(|| "Unknown".into());
    let mut fields = vec![
        field("Status", label),
        field("Victim", &unknown(&report.victim_character_name)),
        field("Ship", &unknown(&report.victim_ship_type)),
        field("Submitted by", &unknown(&report.submitted_by_name)),
    ];
    if report.status == "rejected"
        && let Some(reason) = report.reason.as_deref().filter(|r| !r.is_empty())
    {
        fields.push(field("Reason", reason));
    }
    let (content, allowed_mentions) = if ping_role && !role_id.is_empty() {
        (
            Some(format!("<@&{role_id}>")),
            Some(AllowedMentions {
                parse: Some(vec![]),
                roles: Some(vec![role_id.to_string()]),
            }),
        )
    } else {
        (None, None)
    };
    WebhookPayload {
        content,
        embeds: Some(vec![Embed {
            title: format!("SRP Report — {label}"),
            url: Some(format!(
                "{site_url}/srp-report-detail?id={}",
                report.killmail_id
            )),
            description: None,
            color: status_color(&report.status),
            fields: Some(fields),
        }]),
        allowed_mentions,
    }
}

fn webhook_base_url(webhook_url: &str) -> &str {
    webhook_url
        .split('?')
        .next()
        .unwrap_or(webhook_url)
        .trim_end_matches('/')
}

fn webhook_url_with_wait(webhook_url: &str) -> String {
    if webhook_url.contains('?') {
        format!("{webhook_url}&wait=true")
    } else {
        format!("{webhook_url}?wait=true")
    }
}

async fn check(response: reqwest::Response) -> Result<reqwest::Response, DiscordError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(DiscordError::Api(format!("{status}: {body}")))
}

impl Srp {
    /// Post or edit the Discord notice for a report (a no-op when Discord is disabled).
    pub async fn notify_report(&self, killmail_id: i64) -> Result<(), DiscordError> {
        let discord = &self.settings().discord;
        if !discord.enabled || discord.srp_webhook_url.is_empty() {
            return Ok(());
        }
        let report: NoticeReport = sqlx::query_as(
            "SELECT r.killmail_id, r.status, r.victim_character_name, r.victim_ship_type, \
             r.reason, r.discord_message_id, c.name AS submitted_by_name \
             FROM srp_reports r LEFT JOIN `character` c ON r.submitted_by_id = c.id \
             WHERE r.killmail_id = ?",
        )
        .bind(killmail_id)
        .fetch_one(self.db())
        .await?;
        let webhook = discord.srp_webhook_url.as_str();
        let site = &self.settings().site_url;

        if let Some(message_id) = report.discord_message_id {
            let payload = build_payload(&report, site, false, &discord.srp_ping_role_id);
            let url = format!("{}/messages/{message_id}", webhook_base_url(webhook));
            check(self.http().patch(&url).json(&payload).send().await?).await?;
            return Ok(());
        }

        let payload = build_payload(&report, site, true, &discord.srp_ping_role_id);
        let response = check(
            self.http()
                .post(webhook_url_with_wait(webhook))
                .json(&payload)
                .send()
                .await?,
        )
        .await?;
        let body: MessageResponse = response.json().await?;
        let message_id: i64 = body
            .id
            .parse()
            .map_err(|_| DiscordError::Api(format!("Invalid message id: {}", body.id)))?;
        sqlx::query("UPDATE srp_reports SET discord_message_id = ? WHERE killmail_id = ?")
            .bind(message_id)
            .bind(killmail_id)
            .execute(self.db())
            .await?;
        Ok(())
    }

    /// [`Srp::notify_report`] in the background; failures are logged.
    pub fn spawn_notify_report(&self, killmail_id: i64) {
        let srp = self.clone();
        tokio::spawn(async move {
            if let Err(e) = srp.notify_report(killmail_id).await {
                tracing::error!(killmail_id, error = %e, "could not send the SRP Discord notice");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(status: &str) -> NoticeReport {
        NoticeReport {
            killmail_id: 42,
            status: status.into(),
            victim_character_name: Some("Victim".into()),
            victim_ship_type: None,
            submitted_by_name: Some("Fc".into()),
            reason: Some("no fit".into()),
            discord_message_id: None,
        }
    }

    #[test]
    fn site_url_is_configured_or_derived() {
        assert_eq!(
            site_url("waitlist.example/", "x"),
            "http://waitlist.example"
        );
        assert_eq!(
            site_url("", "https://waitlist.example/auth/cb"),
            "https://waitlist.example/auth"
        );
        assert_eq!(site_url("", ""), "");
    }

    #[test]
    fn payload_matches_the_legacy_notice() {
        let p = build_payload(&report("rejected"), "https://w.example", true, "123");
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["content"], "<@&123>");
        assert_eq!(
            json["allowed_mentions"],
            serde_json::json!({"parse": [], "roles": ["123"]})
        );
        let embed = &json["embeds"][0];
        assert_eq!(embed["title"], "SRP Report — Rejected");
        assert_eq!(embed["url"], "https://w.example/srp-report-detail?id=42");
        assert_eq!(embed["color"], 0xED4245);
        let names: Vec<_> = embed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["Status", "Victim", "Ship", "Submitted by", "Reason"]
        );
        assert_eq!(embed["fields"][2]["value"], "Unknown");

        let p = build_payload(&report("approved"), "https://w.example", false, "123");
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("content").is_none());
        assert_eq!(json["embeds"][0]["fields"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn webhook_urls() {
        assert_eq!(
            webhook_url_with_wait("https://d/w/1/t"),
            "https://d/w/1/t?wait=true"
        );
        assert_eq!(
            webhook_url_with_wait("https://d/w/1/t?thread_id=5"),
            "https://d/w/1/t?thread_id=5&wait=true"
        );
        assert_eq!(
            webhook_base_url("https://d/w/1/t/?thread_id=5"),
            "https://d/w/1/t"
        );
    }
}
