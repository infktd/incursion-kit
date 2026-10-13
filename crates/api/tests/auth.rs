//! Session, whoami, logout, permissions and character authorization.

mod common;

use axum::http::StatusCode;
use common::{TestApp, test_config};
use serde_json::{Value, json};
use wl_api::auth::{AuthenticatedAccount, Session};
use wl_api::{ApiError, AppState};

#[tokio::test]
async fn whoami_requires_a_session() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let res = app.get("/api/auth/whoami", None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.text(), "401 Authorization Required");

    let mut bogus = app.login(1);
    bogus.cookie = "authToken=garbage".into();
    let res = app.get("/api/auth/whoami", Some(&bogus)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.text(), "401 Authorization Required");
}

#[tokio::test]
async fn whoami_lists_main_then_alts_and_access() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let main = app
        .create_account(1001, "Main Pilot", Some("trainee"))
        .await;
    app.create_character(2002, "Alt Two").await;
    app.create_character(2001, "Alt One").await;
    app.link_alt(1001, 2002).await;
    app.link_alt(1001, 2001).await;

    let res = app.get("/api/auth/whoami", Some(&main)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json::<Value>(),
        json!({
            "account_id": 1001,
            "access": [
                "fleet-configure", "fleet-invite", "fleet-view",
                "pilot-view", "waitlist-tag:TRAINEE", "waitlist-view"
            ],
            "characters": [
                {"id": 1001, "name": "Main Pilot"},
                {"id": 2001, "name": "Alt One"},
                {"id": 2002, "name": "Alt Two"}
            ]
        })
    );
}

#[tokio::test]
async fn whoami_for_plain_user_and_unknown_role() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.create_account(1, "User", None).await;
    let res = app.get("/api/auth/whoami", Some(&user)).await;
    assert_eq!(res.json::<Value>()["access"], json!([]));

    let weird = app.create_account(2, "Weird", Some("emperor")).await;
    let res = app.get("/api/auth/whoami", Some(&weird)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.text(), "401 Authorization Required");
}

#[tokio::test]
async fn whoami_for_missing_character_is_database_error() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let ghost = app.login(999);
    let res = app.get("/api/auth/whoami", Some(&ghost)).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(res.text(), "database error");
}

#[tokio::test]
async fn logout_clears_cookie_and_unlinks_alts() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let main = app.create_account(1, "Main", None).await;
    app.create_character(2, "Alt").await;
    app.create_character(3, "Other Main").await;
    app.link_alt(1, 2).await;
    // The account is itself someone else's alt: that link goes too.
    app.link_alt(3, 1).await;
    // Unrelated links stay.
    app.create_character(4, "Unrelated").await;
    app.link_alt(3, 4).await;

    let res = app.get("/api/auth/logout", Some(&main)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.is_empty());
    assert_eq!(
        res.header("set-cookie"),
        Some("authToken=; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400")
    );

    let remaining: Vec<(i64, i64)> =
        sqlx::query_as("SELECT account_id, alt_id FROM alt_character ORDER BY account_id")
            .fetch_all(app.pool())
            .await
            .unwrap();
    assert_eq!(remaining, vec![(3, 4)]);

    // The emptied cookie no longer authenticates.
    let mut cleared = main.clone();
    cleared.cookie = "authToken=".into();
    let res = app.get("/api/auth/whoami", Some(&cleared)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_without_session_and_secure_cookie() {
    let Some(app) = TestApp::with_config(test_config(true)).await else {
        return;
    };
    let res = app.get("/api/auth/logout", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("set-cookie"),
        Some("authToken=; Path=/; HttpOnly; SameSite=Strict; Max-Age=2678400; Secure")
    );
}

async fn account(state: &AppState, id: i64) -> AuthenticatedAccount {
    AuthenticatedAccount::from_session(
        state.db(),
        Session {
            account_id: id,
            window_character_id: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn authorize_character_main_alt_or_override() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    app.create_account(1, "Main", None).await;
    app.create_account(5, "FC", Some("fc")).await;
    app.create_character(2, "Alt").await;
    app.create_character(3, "Stranger").await;
    app.link_alt(1, 2).await;
    let db = app.pool();

    let main = account(&app.state, 1).await;
    assert!(main.authorize_character(db, 1, None).await.is_ok());
    assert!(main.authorize_character(db, 2, None).await.is_ok());
    assert!(matches!(
        main.authorize_character(db, 3, Some("pilot-view")).await,
        Err(ApiError::AccessDenied)
    ));
    // Alts are one-directional: the alt's account does not own the main.
    let alt = account(&app.state, 2).await;
    assert!(matches!(
        alt.authorize_character(db, 1, None).await,
        Err(ApiError::AccessDenied)
    ));

    let fc = account(&app.state, 5).await;
    assert!(
        fc.authorize_character(db, 3, Some("pilot-view"))
            .await
            .is_ok()
    );
    assert!(matches!(
        fc.authorize_character(db, 3, Some("commanders-manage"))
            .await,
        Err(ApiError::AccessDenied)
    ));
    assert!(matches!(
        fc.authorize_character(db, 3, None).await,
        Err(ApiError::AccessDenied)
    ));
}

#[tokio::test]
async fn require_access_uses_role_of_main() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    app.create_account(1, "Admin", Some("admin")).await;
    app.create_account(2, "Trainee", Some("trainee")).await;
    let admin = account(&app.state, 1).await;
    let trainee = account(&app.state, 2).await;
    assert!(admin.require_access("commanders-manage:admin").is_ok());
    assert!(admin.require_access("waitlist-tag:TRAINEE").is_ok());
    assert!(trainee.require_access("fleet-view").is_ok());
    let err = trainee.require_access("waitlist-manage").unwrap_err();
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(err.to_string(), "access denied");
}
