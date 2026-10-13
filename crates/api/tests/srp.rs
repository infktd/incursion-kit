//! SRP routes against a mock ESI (wiremock). Discord notices are disabled here (covered in
//! wl-srp's tests).

mod common;

use common::{Login, TestApp};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};
use wl_esi::{Esi, EsiOptions};
use wl_srp::{Settings, Srp};

const KILLMAIL: i64 = 128982873;
const LINK: &str = "https://esi.evetech.net/latest/killmails/128982873/abcdef/";
const VICTIM: i64 = 3001;
const KILL_TIME: i64 = 1_714_564_800; // 2024-05-01T12:00:00Z

async fn app(server: &MockServer) -> Option<TestApp> {
    let uri = server.uri();
    TestApp::with_services(|state, db, _| {
        let esi = Esi::with_options(
            EsiOptions {
                client_id: "test-client".into(),
                client_secret: "test-secret".into(),
                redirect_url: "http://waitlist.test/auth/cb".into(),
                user_agent: "wl-api tests".into(),
                esi_url: uri.clone(),
                sso_authorize_url: format!("{uri}/v2/oauth/authorize"),
                sso_token_url: format!("{uri}/v2/oauth/token"),
            },
            db.clone(),
        )
        .expect("ESI client");
        let mut settings = Settings::from_config(state.config());
        settings.janice_url = uri.clone();
        let srp = Srp::with_settings(settings, db.clone(), esi.clone());
        state.with_esi(esi).with_srp(srp)
    })
    .await
}

async fn mock_killmail(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path_regex(format!(r"/killmails/{KILLMAIL}/abcdef/?$")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "killmail_id": KILLMAIL,
            "killmail_time": "2024-05-01T12:00:00Z",
            "solar_system_id": 30000142,
            "victim": {
                "character_id": VICTIM,
                "corporation_id": 98000001,
                "damage_taken": 5000,
                "ship_type_id": 17740,
                "items": [{"flag": 27, "item_type_id": 3170, "quantity_destroyed": 1, "singleton": 0}],
            },
            "attackers": [{"damage_done": 5000, "final_blow": true, "security_status": 0.0}],
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/universe/types/17740/?$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "type_id": 17740, "name": "Vindicator", "description": "", "published": true,
            "group_id": 27,
        })))
        .mount(server)
        .await;
}

async fn exec(app: &TestApp, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(app.pool())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn submit(app: &TestApp, fc: &Login) -> common::TestResponse {
    app.post_json(
        "/api/fc/srp/submit",
        Some(fc),
        &json!({"killmail_link": LINK, "description": "died", "loot_returned": false}),
    )
    .await
}

#[tokio::test]
async fn admin_endpoints_need_the_admin_permission() {
    let server = MockServer::start().await;
    let Some(app) = app(&server).await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    let pilot = app.create_account(1002, "Pilot", None).await;
    for path in [
        "/api/admin/srp/setup",
        "/api/admin/srp/service-account",
        "/api/admin/srp/config",
        "/api/admin/srp/all-statuses",
        "/api/admin/srp/incursion-focus",
        "/api/admin/srp/focus-end-timestamp",
        "/api/admin/srp/reports",
        "/api/admin/srp/reports/1",
    ] {
        let res = app.get(path, Some(&fc)).await;
        assert_eq!(
            (res.status.as_u16(), res.text()),
            (401, "access denied"),
            "{path}"
        );
    }
    let res = app
        .post_json("/api/admin/srp/process", Some(&fc), &json!({}))
        .await;
    assert_eq!(res.status, 401);
    let res = app.get("/api/fc/srp/reports", Some(&pilot)).await;
    assert_eq!((res.status.as_u16(), res.text()), (401, "access denied"));
    let res = app.get("/api/fc/srp/reports", None).await;
    assert_eq!(res.status, 401);
    // The legacy unmounted endpoint stays a 404.
    let res = app.get("/api/pilot/srp-reports/1002", Some(&pilot)).await;
    assert_eq!(res.status, 404);
}

#[tokio::test]
async fn pilot_status_and_its_400_quirk() {
    let server = MockServer::start().await;
    let Some(app) = app(&server).await else {
        return;
    };
    let main = app.create_account(1001, "Main", None).await;
    app.create_character(1002, "Alt").await;
    app.link_alt(1001, 1002).await;
    let stranger = app.create_account(1003, "Stranger", None).await;
    let trainee = app
        .create_account(1004, "Trainee", Some("trainee-advanced"))
        .await;
    exec(
        &app,
        "INSERT INTO srp_payments (character_name, payment_amount, payment_date, expires_at, \
         coverage_type, created_at) VALUES ('Alt', 125000000, 1, 4000000000, 'per_focus', 1)",
    )
    .await;

    let res = app
        .get("/api/srp/status?character_id=1001", Some(&main))
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let body: Value = res.json();
    assert_eq!(body["status"], "Paid until end of focus");
    assert_eq!(body["payment_amount"], 125000000.0);
    assert_eq!(body["coverage_type"], "per_focus");
    assert_eq!(body["fc_srp_price_factor"], 0.5);
    assert_eq!(body["srp_tiers"]["daily"][0], 20.0);
    assert_eq!(body["srp_tiers"]["per_focus"].as_array().unwrap().len(), 12);

    let res = app
        .get("/api/srp/status?character_id=1002", Some(&main))
        .await;
    assert_eq!(res.status, 200);
    let res = app
        .get("/api/srp/status?character_id=1001", Some(&stranger))
        .await;
    assert_eq!((res.status.as_u16(), res.text()), (400, "Unauthorized"));
    let res = app
        .get("/api/srp/status?character_id=1003", Some(&trainee))
        .await;
    let body: Value = res.json();
    assert_eq!(body["status"], "Unpaid");
    assert_eq!(body["payment_amount"], Value::Null);
}

#[tokio::test]
async fn report_lifecycle() {
    let server = MockServer::start().await;
    mock_killmail(&server).await;
    let Some(app) = app(&server).await else {
        return;
    };
    let fc = app.create_account(1001, "Fc", Some("fc")).await;
    let other_fc = app.create_account(1002, "Other Fc", Some("fc")).await;
    let admin = app.create_account(1003, "Admin", Some("admin")).await;
    app.create_character(VICTIM, "Victim").await;
    exec(
        &app,
        &format!(
            "INSERT INTO srp_payments (character_name, payment_amount, payment_date, expires_at, \
             coverage_type, created_at) VALUES ('Victim', 20000000, {}, {}, 'daily', 0)",
            KILL_TIME - 60,
            KILL_TIME + 3600
        ),
    )
    .await;
    exec(
        &app,
        &format!(
            "INSERT INTO fleet_activity (character_id, fleet_id, first_seen, last_seen, hull, \
             has_left, is_boss) VALUES ({VICTIM}, 1, {}, {}, 17740, 0, 0)",
            KILL_TIME - 600,
            KILL_TIME + 600
        ),
    )
    .await;

    let res = submit(&app, &fc).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "message": "SRP report submitted successfully"})
    );
    let res = submit(&app, &fc).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (
            400,
            "An SRP report for this killmail already exists. Use the update button to modify existing reports."
        )
    );
    let res = app
        .post_json(
            "/api/fc/srp/submit",
            Some(&fc),
            &json!({"killmail_link": "https://zkillboard.com/kill/1/", "description": "", "loot_returned": false}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Invalid killmail link: Invalid killmail URL format")
    );

    // FCs see every report, without links unless configured.
    let reports: Value = app.get("/api/fc/srp/reports", Some(&other_fc)).await.json();
    let report = &reports["reports"][0];
    assert_eq!(report["killmail_link"], "");
    assert_eq!(report["status"], "pending");
    assert_eq!(report["submitted_by_name"], "Fc");
    assert_eq!(report["victim_character_name"], "Victim");
    assert_eq!(report["victim_ship_type"], "Vindicator");
    assert_eq!(report["loot_returned"], false);
    let srp_paid: Value = serde_json::from_str(report["srp_paid"].as_str().unwrap()).unwrap();
    assert_eq!(
        srp_paid,
        json!({
            "had_coverage": true,
            "payment_date": "2024-05-01 11:59 UTC",
            "payment_amount": "20000000.00",
            "coverage_character": "Victim",
            "coverage_type": "direct",
        })
    );
    let fleet_comp: Option<String> = sqlx::query_scalar(
        "SELECT CAST(fleet_comp AS CHAR) FROM srp_reports WHERE killmail_id = ?",
    )
    .bind(KILLMAIL)
    .fetch_one(app.pool())
    .await
    .unwrap();
    assert_eq!(fleet_comp.as_deref(), Some(r#"{"Vindicator": 1}"#));

    let res = app
        .post_json(
            "/api/admin/srp/config",
            Some(&admin),
            &json!({"key": "show_fc_killmail_links", "value": "1"}),
        )
        .await;
    assert_eq!(res.text(), "Configuration updated successfully");
    let config: Value = app.get("/api/admin/srp/config", Some(&admin)).await.json();
    assert_eq!(config, json!({"config": {"show_fc_killmail_links": "1"}}));
    let reports: Value = app.get("/api/fc/srp/reports", Some(&other_fc)).await.json();
    assert_eq!(reports["reports"][0]["killmail_link"], LINK);

    // Editing: the submitter or an admin.
    let path = format!("/api/fc/srp/update/{KILLMAIL}");
    let edit = json!({"description": "edited", "loot_returned": true});
    let res = app.post_json(&path, Some(&other_fc), &edit).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (403, "You can only update SRP reports that you submitted")
    );
    let res = app.post_json(&path, Some(&fc), &edit).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let res = app
        .get(&format!("/api/fc/srp/report/{KILLMAIL}"), Some(&other_fc))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (403, "You can only edit SRP reports that you submitted")
    );
    let res = app
        .get(&format!("/api/fc/srp/report/{KILLMAIL}"), Some(&admin))
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "report": {
            "killmail_id": KILLMAIL, "killmail_link": LINK, "description": "edited",
            "loot_returned": true,
        }})
    );
    let res = app.get("/api/fc/srp/report/5", Some(&fc)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "SRP report not found")
    );

    // Admin views.
    let detail: Value = app
        .get(&format!("/api/admin/srp/reports/{KILLMAIL}"), Some(&admin))
        .await
        .json();
    assert_eq!(detail["report"]["description"], "edited");
    let res = app.get("/api/admin/srp/reports/5", Some(&admin)).await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (404, "SRP report not found")
    );
    let km: Value = app
        .get(
            &format!("/api/admin/srp/reports/{KILLMAIL}/killmail"),
            Some(&admin),
        )
        .await
        .json();
    assert_eq!(km["victim"]["character_id"], VICTIM);
    let fleet: Value = app
        .get(
            &format!("/api/admin/srp/reports/{KILLMAIL}/fleet-validation"),
            Some(&admin),
        )
        .await
        .json();
    assert_eq!(fleet, json!({"was_in_fleet": true}));
    let validation: Value = app
        .get(
            &format!("/api/admin/srp/reports/{KILLMAIL}/srp-validation"),
            Some(&admin),
        )
        .await
        .json();
    assert_eq!(
        validation,
        json!({
            "had_srp_coverage": true,
            "alt_needs_linking": null,
            "payment_date": "2024-05-01 11:59 UTC",
            "payment_amount": 20000000.0,
            "expires_at": "2024-05-01 13:00 UTC",
            "coverage_character": "Victim",
            "coverage_type": "direct",
        })
    );

    // Decisions.
    let res = app
        .post_json(
            &format!("/api/admin/srp/reports/{KILLMAIL}/approve"),
            Some(&admin),
            &json!({"payout_amount": 150000000.0}),
        )
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"success": true, "message": "SRP report approved successfully"})
    );
    let detail: Value = app
        .get(&format!("/api/admin/srp/reports/{KILLMAIL}"), Some(&admin))
        .await
        .json();
    assert_eq!(detail["report"]["status"], "approved");
    assert_eq!(detail["report"]["payout_amount"], 150000000.0);
    let res = app
        .post_json(
            &format!("/api/admin/srp/reports/{KILLMAIL}/deny"),
            Some(&admin),
            &json!({"reason": "no fit"}),
        )
        .await;
    assert_eq!(res.status, 200);
    let reports: Value = app.get("/api/admin/srp/reports", Some(&admin)).await.json();
    assert_eq!(reports["reports"][0]["status"], "rejected");
    assert_eq!(reports["reports"][0]["reason"], "no fit");
    let res = app
        .post_json(
            "/api/admin/srp/reports/5/deny",
            Some(&admin),
            &json!({"reason": "x"}),
        )
        .await;
    assert_eq!(res.status, 404);

    // A paid report's decision is final.
    exec(
        &app,
        &format!("UPDATE srp_reports SET status = 'paid' WHERE killmail_id = {KILLMAIL}"),
    )
    .await;
    for (action, body) in [
        ("approve", json!({"payout_amount": 1.0})),
        ("deny", json!({"reason": "x"})),
    ] {
        let res = app
            .post_json(
                &format!("/api/admin/srp/reports/{KILLMAIL}/{action}"),
                Some(&admin),
                &body,
            )
            .await;
        assert_eq!(
            (res.status.as_u16(), res.text()),
            (400, "SRP report is already paid")
        );
    }

    // In-game windows need the admin's open-window token.
    let res = app
        .post_json(
            &format!("/api/admin/srp/reports/{KILLMAIL}/open-victim-window"),
            Some(&admin),
            &json!({}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "Failed to open character window: no ESI token found")
    );
}

#[tokio::test]
async fn service_account_setup_and_processing() {
    let server = MockServer::start().await;
    let Some(app) = app(&server).await else {
        return;
    };
    let admin = app.create_account(1003, "Admin", Some("admin")).await;

    let res = app.get("/api/admin/srp/setup", Some(&admin)).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert!(res.header("set-cookie").is_some());
    let body: Value = res.json();
    assert_eq!(body["has_service_account"], false);
    let url = body["login_url"].as_str().unwrap();
    assert!(
        url.contains("state=srp_setup%3A") || url.contains("state=srp_setup:"),
        "{url}"
    );
    assert!(
        url.contains("esi-wallet.read_corporation_wallets.v1"),
        "{url}"
    );

    let res = app
        .get("/api/admin/srp/service-account", Some(&admin))
        .await;
    assert_eq!(res.json::<Value>(), json!({"service_account": null}));
    let res = app
        .post_json("/api/admin/srp/test", Some(&admin), &json!({}))
        .await;
    assert_eq!(
        res.json::<Value>(),
        json!({"success": false, "message": "No service account configured", "entries_found": null})
    );
    let res = app
        .post_json("/api/admin/srp/process", Some(&admin), &json!({}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (400, "No SRP service account configured")
    );

    // A configured account whose corporation wallet lists two divisions.
    app.create_character(9001, "Wallet Reader").await;
    exec(
        &app,
        "INSERT INTO access_token (character_id, access_token, expires, scopes) VALUES \
         (9001, 'token', 4000000000, 'publicData esi-wallet.read_corporation_wallets.v1 ')",
    )
    .await;
    wl_srp::service_account::store_service_account_tokens(
        app.pool(),
        &wl_srp::service_account::ServiceAccountTokens {
            character_id: 9001,
            character_name: "Wallet Reader",
            corporation_id: 98000001,
            wallet_id: 1,
            access_token: "token",
            refresh_token: "refresh",
            expires: 0,
            scopes: wl_srp::service_account::SERVICE_ACCOUNT_SCOPES,
        },
    )
    .await
    .unwrap();
    Mock::given(method("GET"))
        .and(path_regex(r"/corporations/98000001/wallets/?$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"division": 1, "balance": 10.5}, {"division": 2, "balance": 0.0}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/corporations/98000001/wallets/1/journal/?$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": 1, "amount": 20000000.0, "balance": 20000000.0, "date": chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "description": "Pilot deposited cash into Corp's account", "ref_type": "player_donation",
        }])))
        .mount(&server)
        .await;

    let sa: Value = app
        .get("/api/admin/srp/service-account", Some(&admin))
        .await
        .json();
    assert_eq!(sa["service_account"]["character_name"], "Wallet Reader");
    assert_eq!(sa["service_account"]["wallet_id"], 1);
    assert_eq!(sa["service_account"]["is_active"], true);
    let res: Value = app
        .post_json("/api/admin/srp/test", Some(&admin), &json!({}))
        .await
        .json();
    assert_eq!(res["success"], true);
    assert_eq!(res["entries_found"], 1);
    let res: Value = app
        .post_json("/api/admin/srp/wallets", Some(&admin), &json!({}))
        .await
        .json();
    assert_eq!(
        res,
        json!({"success": true, "message": "Found 2 available corporation wallets",
               "wallets": [{"division": 1, "balance": 10.5}, {"division": 2, "balance": 0.0}]})
    );
    let res: Value = app
        .post_json("/api/admin/srp/journal", Some(&admin), &json!({}))
        .await
        .json();
    assert_eq!(res["wallet_id"], 1);
    assert_eq!(res["entries"][0]["first_party_type"], Value::Null);
    assert_eq!(res["entries"][0]["ref_type"], "player_donation");

    let res = app
        .post_json("/api/admin/srp/process", Some(&admin), &json!({}))
        .await;
    assert_eq!(
        (res.status.as_u16(), res.text()),
        (200, "SRP payments processed successfully")
    );
    let all: Value = app
        .get("/api/admin/srp/all-statuses", Some(&admin))
        .await
        .json();
    assert_eq!(all["fc_srp_price_factor"], 0.5);
    assert_eq!(all["statuses"][0]["character_name"], "Pilot");
    assert_eq!(all["statuses"][0]["payment_amount"], 20000000.0);
    assert_eq!(all["statuses"][0]["coverage_type"], "daily");
    assert_eq!(all["statuses"][0]["focus_voided_at"], Value::Null);

    let res = app
        .post_json("/api/admin/srp/remove", Some(&admin), &json!({}))
        .await;
    assert_eq!(res.json::<Value>()["success"], true);
    let res = app
        .get("/api/admin/srp/service-account", Some(&admin))
        .await;
    assert_eq!(res.json::<Value>(), json!({"service_account": null}));
}

#[tokio::test]
async fn focus_endpoints() {
    let server = MockServer::start().await;
    let Some(app) = app(&server).await else {
        return;
    };
    let admin = app.create_account(1003, "Admin", Some("admin")).await;
    let res: Value = app
        .get("/api/admin/srp/focus-end-timestamp", Some(&admin))
        .await
        .json();
    assert_eq!(
        res,
        json!({"focus_end_timestamp": null, "formatted_date": null})
    );
    let res: Value = app
        .get("/api/admin/srp/incursion-focus", Some(&admin))
        .await
        .json();
    assert_eq!(res, json!({"focus_status": null}));

    exec(
        &app,
        "INSERT INTO incursion_focus (current_focus_constellation_id, \
         current_focus_constellation_name, last_check_timestamp, focus_active, created_at, \
         updated_at, focus_end_timestamp) VALUES (20000001, 'Somewhere', 5, 0, 1, 2, 1714564800)",
    )
    .await;
    let res: Value = app
        .get("/api/admin/srp/focus-end-timestamp", Some(&admin))
        .await
        .json();
    assert_eq!(
        res,
        json!({"focus_end_timestamp": 1714564800, "formatted_date": "2024-05-01 12:00:00 UTC"})
    );
    let res: Value = app
        .get("/api/admin/srp/incursion-focus", Some(&admin))
        .await
        .json();
    assert_eq!(
        res["focus_status"]["current_focus_constellation_name"],
        "Somewhere"
    );
    assert_eq!(res["focus_status"]["focus_active"], false);
}
