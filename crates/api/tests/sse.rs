//! `GET /api/sse/stream`.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{Login, TestApp};
use futures::StreamExt;
use tower::ServiceExt;
use wl_live::{events, topics};

/// Open the stream and return its body as a stream of text chunks.
async fn connect(
    app: &TestApp,
    login: Option<&Login>,
) -> (
    StatusCode,
    axum::http::HeaderMap,
    axum::body::BodyDataStream,
) {
    let request = TestApp::build(Method::GET, "/api/sse/stream", login)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = response.into_body().into_data_stream();
    if status == StatusCode::OK {
        assert_eq!(next_chunk(&mut body).await, OPENED, "the opening comment");
    }
    (status, headers, body)
}

/// What every stream sends first, right away.
const OPENED: &str = ": connected\n\n";

async fn next_chunk(body: &mut axum::body::BodyDataStream) -> String {
    let chunk = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .expect("timed out waiting for an event")
        .expect("stream ended")
        .expect("body error");
    String::from_utf8(chunk.to_vec()).unwrap()
}

async fn wait_for_subscribers(app: &TestApp, n: usize) {
    for _ in 0..100 {
        if app.live().subscriber_count() == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} subscribers");
}

/// Both streams send a comment as soon as they open (buffering proxies otherwise hold the
/// response until the first keep-alive, 15 seconds later).
#[tokio::test]
async fn streams_open_with_a_comment_right_away() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.create_account(1, "User", None).await;
    for (path, login) in [
        ("/api/sse/stream", Some(&user)),
        ("/api/incursions/stream", None),
    ] {
        let request = TestApp::build(Method::GET, path, login)
            .body(axum::body::Body::empty())
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let mut body = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .unwrap_or_else(|_| panic!("{path}: nothing sent on open"))
            .expect("stream open")
            .unwrap();
        assert_eq!(String::from_utf8(first.to_vec()).unwrap(), OPENED, "{path}");
    }
}

#[tokio::test]
async fn requires_a_session() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let (status, _, _) = connect(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn streams_account_topics() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let user = app.create_account(1, "User", None).await;
    let (status, headers, mut body) = connect(&app, Some(&user)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "text/event-stream");
    wait_for_subscribers(&app, 1).await;

    let live = app.live();
    // Not subscribed without fleet-view, nor to other accounts.
    live.publish(topics::FLEET_COMP, events::COMP_UPDATE, r#"{"fleet_id":1}"#);
    live.publish(&topics::account(2), events::WAKEUP, "not for you");

    live.publish(
        &topics::account(1),
        events::WAKEUP,
        "Bob has invited your Vindicator to fleet.",
    );
    assert_eq!(
        next_chunk(&mut body).await,
        "event: wakeup\ndata: Bob has invited your Vindicator to fleet.\n\n"
    );
    live.publish_json(
        topics::WAITLIST,
        events::WAITLIST_UPDATE,
        &serde_json::json!({"waitlist_id": 1}),
    )
    .unwrap();
    assert_eq!(
        next_chunk(&mut body).await,
        "event: waitlist_update\ndata: {\"waitlist_id\":1}\n\n"
    );
    live.publish(topics::ANNOUNCEMENTS, events::ANNOUNCEMENT_NEW, "[]");
    assert_eq!(
        next_chunk(&mut body).await,
        "event: announcment;new\ndata: []\n\n"
    );

    drop(body);
    wait_for_subscribers(&app, 0).await;
}

#[tokio::test]
async fn fleet_view_adds_fleet_comp() {
    let Some(app) = TestApp::new().await else {
        return;
    };
    let fc = app.create_account(5, "FC", Some("trainee")).await;
    let (_, _, mut body) = connect(&app, Some(&fc)).await;
    wait_for_subscribers(&app, 1).await;
    app.live()
        .publish(topics::FLEET_COMP, events::COMP_UPDATE, r#"{"fleet_id":7}"#);
    assert_eq!(
        next_chunk(&mut body).await,
        "event: comp_update\ndata: {\"fleet_id\":7}\n\n"
    );
}
