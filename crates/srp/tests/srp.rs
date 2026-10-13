//! wl-srp against MySQL (`TEST_DATABASE_URL`; skipped without it) and mock ESI, Discord and
//! Janice servers (wiremock).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use wiremock::matchers::{body_string, header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_core::config::{DiscordConfig, SrpConfig};
use wl_db::test_support::TestDb;
use wl_esi::{Esi, EsiOptions};
use wl_srp::{Settings, Srp, focus};

const CORP: i64 = 98000001;
const SERVICE: i64 = 9001;

struct Fixture {
    db: TestDb,
    srp: Srp,
    server: MockServer,
}

fn settings(server: &MockServer, discord: bool) -> Settings {
    Settings {
        tiers: SrpConfig::default(),
        fc_price_factor: 0.5,
        interval_seconds: 300,
        janice_api_key: "janice-key".into(),
        janice_url: server.uri(),
        discord: DiscordConfig {
            enabled: discord,
            srp_webhook_url: format!("{}/api/webhooks/1/token", server.uri()),
            site_url: "https://waitlist.test".into(),
            srp_ping_role_id: "777".into(),
        },
        site_url: "https://waitlist.test".into(),
    }
}

async fn fixture(discord: bool) -> Option<Fixture> {
    let db = TestDb::new().await?;
    let server = MockServer::start().await;
    let esi = Esi::with_options(
        EsiOptions {
            client_id: "id".into(),
            client_secret: "secret".into(),
            redirect_url: "https://waitlist.test/auth/cb".into(),
            user_agent: "wl-srp tests".into(),
            esi_url: server.uri(),
            sso_authorize_url: format!("{}/v2/oauth/authorize", server.uri()),
            sso_token_url: format!("{}/v2/oauth/token", server.uri()),
        },
        db.pool().clone(),
    )
    .unwrap();
    let srp = Srp::with_settings(settings(&server, discord), db.pool().clone(), esi);
    Some(Fixture { db, srp, server })
}

impl Fixture {
    async fn exec(&self, sql: &str) {
        sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
            .execute(self.db.pool())
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    async fn character(&self, id: i64, name: &str) {
        self.exec(&format!(
            "INSERT INTO `character` (id, name) VALUES ({id}, '{name}')"
        ))
        .await;
    }

    async fn service_account(&self) {
        self.character(SERVICE, "Wallet Reader").await;
        self.exec(&format!(
            "INSERT INTO access_token (character_id, access_token, expires, scopes) VALUES \
             ({SERVICE}, 'token', 4000000000, 'publicData esi-wallet.read_corporation_wallets.v1 ')"
        ))
        .await;
        wl_srp::service_account::store_service_account_tokens(
            self.db.pool(),
            &wl_srp::service_account::ServiceAccountTokens {
                character_id: SERVICE,
                character_name: "Wallet Reader",
                corporation_id: CORP,
                wallet_id: 1,
                access_token: "token",
                refresh_token: "refresh",
                expires: 0,
                scopes: wl_srp::service_account::SERVICE_ACCOUNT_SCOPES,
            },
        )
        .await
        .unwrap();
    }

    async fn journal(&self, entries: Value) {
        Mock::given(method("GET"))
            .and(path_regex(format!(
                r"/corporations/{CORP}/wallets/1/journal/?$"
            )))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(entries))
            .mount(&self.server)
            .await;
    }

    async fn payments(&self) -> Vec<(String, f64, String, i64)> {
        sqlx::query_as(
            "SELECT character_name, CAST(payment_amount AS DOUBLE), coverage_type, expires_at \
             FROM srp_payments ORDER BY character_name, payment_date",
        )
        .fetch_all(self.db.pool())
        .await
        .unwrap()
    }

    async fn report_status(&self, killmail_id: i64) -> (String, Option<i64>) {
        sqlx::query_as("SELECT status, payout_date FROM srp_reports WHERE killmail_id = ?")
            .bind(killmail_id)
            .fetch_one(self.db.pool())
            .await
            .unwrap()
    }
}

fn iso(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn entry(id: i64, amount: f64, date: DateTime<Utc>, description: &str) -> Value {
    json!({
        "id": id,
        "amount": amount,
        "balance": 1.0,
        "date": iso(date),
        "description": description,
        "ref_type": if amount > 0.0 { "player_donation" } else { "corporation_account_withdrawal" },
    })
}

#[tokio::test]
async fn reconciliation_rebuilds_payments_and_marks_payouts_paid() {
    let Some(f) = fixture(false).await else {
        return;
    };
    f.service_account().await;
    f.character(1001, "Pilot One").await;
    f.character(1002, "Fc Main").await;
    f.character(1003, "Fc Alt").await;
    f.exec("INSERT INTO admin VALUES (1002, 'fc', 0, 1002)")
        .await;
    f.exec("INSERT INTO alt_character VALUES (1002, 1003)")
        .await;

    let now = Utc::now();
    let hour = chrono::Duration::hours(1);
    // Two approved reports for the same victim and amount: one payout pays exactly one.
    for (id, submitted) in [(501, now - hour * 30), (502, now - hour * 20)] {
        f.exec(&format!(
            "INSERT INTO srp_reports (killmail_id, killmail_link, submitted_at, submitted_by_id, \
             status, payout_amount, victim_character_name) VALUES ({id}, 'link{id}', {}, 1002, \
             'approved', 150000000, 'Pilot One')",
            submitted.timestamp()
        ))
        .await;
    }
    let payout_at = now - hour * 2;
    f.journal(json!([
        entry(
            1,
            20_000_000.0,
            now - hour,
            "Pilot One deposited cash into Corp's account"
        ),
        // FC price (half of 125M) paid by an FC's alt: per-focus.
        entry(
            2,
            62_500_000.0,
            now - hour * 3,
            "Fc Alt transferred cash from Fc Alt's account to Corp's account"
        ),
        // FC price paid by a non-FC: ignored.
        entry(
            3,
            10_000_000.0,
            now - hour * 4,
            "Pilot One deposited cash into Corp's account"
        ),
        // Not a tier price.
        entry(
            4,
            21_000_000.0,
            now - hour * 5,
            "Pilot One deposited cash into Corp's account"
        ),
        // Older than the 8-day window.
        entry(
            5,
            20_000_000.0,
            now - chrono::Duration::days(9),
            "Old Pilot deposited cash into Corp's account"
        ),
        entry(
            6,
            -150_000_000.0,
            payout_at,
            "Corp transferred cash from Corp's account to Pilot One's account"
        ),
    ]))
    .await;

    let outcome = f.srp.process_payments().await.unwrap();
    assert_eq!(outcome.payments, 2);
    assert_eq!(outcome.paid_reports, vec![501]);
    let payments = f.payments().await;
    assert_eq!(payments.len(), 2);
    assert_eq!(
        (
            payments[0].0.as_str(),
            payments[0].1,
            payments[0].2.as_str()
        ),
        ("Fc Alt", 62_500_000.0, "per_focus")
    );
    assert_eq!(
        payments[0].3,
        (now - hour * 3).timestamp() + 8 * 86400,
        "per-focus coverage lasts 8 days"
    );
    assert_eq!(
        (payments[1].0.as_str(), payments[1].2.as_str()),
        ("Pilot One", "daily")
    );
    assert_eq!(
        f.report_status(501).await,
        ("paid".into(), Some(payout_at.timestamp()))
    );
    assert_eq!(f.report_status(502).await, ("approved".into(), None));

    // A second run rebuilds the same payments and doesn't pay the second report.
    let outcome = f.srp.process_payments().await.unwrap();
    assert_eq!(outcome.payments, 2);
    assert!(outcome.paid_reports.is_empty());
    assert_eq!(f.payments().await.len(), 2);
    assert_eq!(f.report_status(502).await, ("approved".into(), None));

    let last_used: Option<i64> = sqlx::query_scalar("SELECT last_used FROM srp_service_account")
        .fetch_one(f.db.pool())
        .await
        .unwrap();
    assert!(last_used.is_some());

    // Coverage is shared across the account: the FC main is covered by the alt's payment.
    let status = f.srp.pilot_status(1002).await.unwrap();
    assert_eq!(status.status.as_deref(), Some("Paid until end of focus"));
    assert_eq!(status.coverage_type.as_deref(), Some("per_focus"));
    let status = f.srp.pilot_status(1001).await.unwrap();
    assert!(status.status.unwrap().starts_with("Paid until 20"));
    assert_eq!(status.payment_amount, Some(20_000_000.0));
    assert_eq!(
        f.srp.pilot_status(424242).await.unwrap().status.as_deref(),
        Some("Unpaid")
    );

    // The focus ends: per-focus coverage bought before it is voided.
    f.exec(&format!(
        "INSERT INTO incursion_focus (last_check_timestamp, focus_active, created_at, \
         updated_at, focus_end_timestamp) VALUES (0, 0, 0, 0, {})",
        now.timestamp()
    ))
    .await;
    assert_eq!(focus::check_srp_validity(f.db.pool()).await.unwrap(), 1);
    assert_eq!(focus::check_srp_validity(f.db.pool()).await.unwrap(), 0);
    assert_eq!(
        f.srp.pilot_status(1002).await.unwrap().status.as_deref(),
        Some("Unpaid")
    );
}

#[tokio::test]
async fn processing_needs_a_service_account() {
    let Some(f) = fixture(false).await else {
        return;
    };
    let err = f.srp.process_payments().await.unwrap_err();
    assert_eq!(err.to_string(), "No SRP service account configured");
    // The updater just skips.
    wl_srp::updater::run_once(&f.srp).await.unwrap();
}

#[tokio::test]
async fn coverage_at_killmail_time_checks_main_and_alts() {
    let Some(f) = fixture(false).await else {
        return;
    };
    f.character(2001, "Main").await;
    f.character(2002, "Alt A").await;
    f.character(2003, "Alt B").await;
    f.exec("INSERT INTO alt_character VALUES (2001, 2002), (2001, 2003)")
        .await;
    f.exec(
        "INSERT INTO srp_payments (character_name, payment_amount, payment_date, expires_at, \
         coverage_type, created_at, focus_voided_at) VALUES \
         ('Alt B', 125000000, 1000, 5000, 'per_focus', 0, NULL), \
         ('Main', 20000000, 6000, 7000, 'daily', 0, 6500)",
    )
    .await;

    // Alt A is covered by its sibling Alt B (admin validation).
    let at = f.srp.coverage_at(Some(2002), 2000).await.unwrap();
    assert!(at.had_srp_coverage);
    assert_eq!(at.coverage_type.as_deref(), Some("alt"));
    assert_eq!(at.coverage_character.as_deref(), Some("Alt B"));
    assert_eq!(at.payment_amount, Some(125_000_000.0));
    assert_eq!(at.payment_date.as_deref(), Some("1970-01-01 00:16 UTC"));

    // The main's voided payment still counts for the admin validation...
    let at = f.srp.coverage_at(Some(2002), 6800).await.unwrap();
    assert_eq!(at.coverage_type.as_deref(), Some("main"));
    // ...but not for the submit-time snapshot.
    assert_eq!(
        f.srp.coverage_snapshot(Some(2002), 6800).await.unwrap(),
        json!({"had_coverage": false})
    );
    assert_eq!(
        f.srp.coverage_snapshot(Some(2001), 2000).await.unwrap(),
        json!({
            "had_coverage": true,
            "payment_date": "1970-01-01 00:16 UTC",
            "payment_amount": "125000000.00",
            "coverage_character": "Alt B",
            "coverage_type": "alt",
        })
    );

    let unknown = f.srp.coverage_at(Some(999), 2000).await.unwrap();
    assert_eq!(unknown.alt_needs_linking, Some(true));
    assert!(
        !f.srp
            .coverage_at(None, 2000)
            .await
            .unwrap()
            .had_srp_coverage
    );
}

#[tokio::test]
async fn discord_notice_is_posted_then_edited() {
    let Some(f) = fixture(true).await else {
        return;
    };
    f.character(1002, "Fc Main").await;
    f.exec(
        "INSERT INTO srp_reports (killmail_id, killmail_link, submitted_at, submitted_by_id, \
         status, victim_character_name, victim_ship_type) \
         VALUES (77, 'link', 0, 1002, 'pending', 'Victim', 'Vindicator')",
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/webhooks/1/token"))
        .and(query_param("wait", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "123456789"})))
        .expect(1)
        .mount(&f.server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/webhooks/1/token/messages/123456789"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "123456789"})))
        .expect(1)
        .mount(&f.server)
        .await;

    f.srp.notify_report(77).await.unwrap();
    let stored: Option<i64> =
        sqlx::query_scalar("SELECT discord_message_id FROM srp_reports WHERE killmail_id = 77")
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(stored, Some(123456789));
    f.exec("UPDATE srp_reports SET status = 'rejected', reason = 'no fit' WHERE killmail_id = 77")
        .await;
    f.srp.notify_report(77).await.unwrap();

    let requests = f.server.received_requests().await.unwrap();
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(first["content"], "<@&777>");
    assert_eq!(first["embeds"][0]["title"], "SRP Report — Pending");
    assert_eq!(
        first["embeds"][0]["url"],
        "https://waitlist.test/srp-report-detail?id=77"
    );
    let edit: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert!(edit.get("content").is_none());
    assert_eq!(edit["embeds"][0]["title"], "SRP Report — Rejected");
    assert_eq!(edit["embeds"][0]["fields"][4]["value"], "no fit");
}

#[tokio::test]
async fn appraisal_calls_janice_and_applies_overrides() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/rest/v2/appraisal"))
        .and(query_param("market", "2"))
        .and(query_param("pricing", "sell"))
        .and(header("X-ApiKey", "janice-key"))
        .and(body_string("Item A 2\nItem B 1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"amount": 2, "itemType": {"eid": 10, "name": "Item A"},
                 "effectivePrices": {"sellPrice": 100.0, "sellPriceTotal": 200.0}},
                {"amount": 1, "itemType": {"eid": 11, "name": "Item B"},
                 "effectivePrices": {"sellPrice": 50.0, "sellPriceTotal": 50.0}},
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/rest/v2/appraisal"))
        .and(body_string("Bad"))
        .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
        .mount(&server)
        .await;

    // No database needed for the appraisal.
    let pool = sqlx::MySqlPool::connect_lazy("mysql://nobody@127.0.0.1:1/none").unwrap();
    let esi = Esi::with_options(
        EsiOptions {
            client_id: "id".into(),
            client_secret: "secret".into(),
            redirect_url: "https://waitlist.test/auth/cb".into(),
            user_agent: "wl-srp tests".into(),
            esi_url: server.uri(),
            sso_authorize_url: server.uri(),
            sso_token_url: server.uri(),
        },
        pool.clone(),
    )
    .unwrap();
    let srp = Srp::with_settings(settings(&server, false), pool, esi);
    let mut rules = BTreeMap::new();
    rules.insert(
        10,
        wl_fitting::SrpModifiedRule {
            pricing: wl_fitting::SrpModifiedPricing::MarketPercent(25.0),
            hulls: Default::default(),
            exclude_hulls: Default::default(),
        },
    );
    let (total, items) = srp
        .appraise(&["Item A 2".into(), "Item B 1".into()], None, &rules, None)
        .await
        .unwrap();
    assert_eq!(total, 100.0);
    assert_eq!(items[0]["srp_override"]["market_percent"], 25.0);
    assert_eq!(items[0]["effectivePrices"]["sellPriceTotal"], 50.0);

    let err = srp
        .appraise(&["Bad".into()], None, &rules, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("Janice API returned error: 400"),
        "{err}"
    );
    assert_eq!(
        srp.appraise(&[], None, &rules, None).await.unwrap(),
        (0.0, vec![])
    );
}
