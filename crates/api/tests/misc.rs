//! Health check, fallbacks and middleware.

mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::TestApp;

#[tokio::test]
async fn healthz_answers_ok() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let res = app.get("/healthz", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text(), "OK");
    assert!(
        res.header("x-request-id").is_some(),
        "request id is propagated"
    );
}

#[tokio::test]
async fn unknown_routes_and_methods_are_plain_404() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let res = app.get("/api/does-not-exist", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.text(), "404 Not Found");

    let res = app
        .request(
            TestApp::build(Method::POST, "/api/auth/whoami", None)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.text(), "404 Not Found");
}

#[tokio::test]
async fn keeps_incoming_request_id() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let res = app
        .request(
            TestApp::build(Method::GET, "/healthz", None)
                .header("x-request-id", "abc-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.header("x-request-id"), Some("abc-123"));
}
