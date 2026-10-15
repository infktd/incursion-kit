//! The wiki API (`/api/wiki*`): permissions, visibility, concurrency, revisions, validation,
//! search, images, import and live events.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, header};
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wl_live::topics;

use common::{Login, TestApp, TestResponse};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../wiki/tests/fixtures/content")
}

struct Wiki {
    app: TestApp,
    admin: Login,
    fcplus: Login,
    fc: Login,
    trainee: Login,
    user: Login,
}

async fn setup() -> Option<Wiki> {
    let app = TestApp::new().await?;
    let admin = app.create_account(9001, "Admin", Some("admin")).await;
    let fcplus = app.create_account(9002, "Senior FC", Some("fc+")).await;
    let fc = app.create_account(9003, "FC", Some("fc")).await;
    let trainee = app.create_account(9004, "Trainee", Some("trainee")).await;
    let user = app.create_account(9005, "Pilot", None).await;
    let res = app
        .post_json(
            "/api/wiki/sections",
            Some(&fcplus),
            &json!({"key": "start", "label": "Start here"}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let res = app
        .post_json(
            "/api/wiki/sections",
            Some(&fcplus),
            &json!({"key": "fc", "label": "FC team"}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    Some(Wiki {
        app,
        admin,
        fcplus,
        fc,
        trainee,
        user,
    })
}

async fn create(w: &Wiki, login: &Login, page: Value) -> TestResponse {
    w.app.post_json("/api/wiki/pages", Some(login), &page).await
}

async fn create_ok(w: &Wiki, page: Value) -> Value {
    let res = create(w, &w.fc, page).await;
    assert_eq!(res.status, 201, "{}", res.text());
    res.json()
}

async fn put(w: &Wiki, login: &Login, id: i64, body: Value) -> TestResponse {
    w.app
        .send_json(
            Method::PUT,
            &format!("/api/wiki/pages/{id}"),
            Some(login),
            &body,
        )
        .await
}

fn error(res: &TestResponse) -> String {
    res.json::<Value>()["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn permissions_and_json_errors() {
    let Some(w) = setup().await else { return };
    let page = json!({"slug": "guides/a", "title": "A", "section": "start", "body": "Hi"});

    // Anonymous and users without wiki-edit: JSON errors.
    let res = create(&w, &w.trainee, page.clone()).await;
    assert_eq!(res.status, 401);
    assert_eq!(error(&res), "access_denied");
    assert!(
        res.header("content-type")
            .unwrap()
            .starts_with("application/json")
    );
    let res = w.app.post_json("/api/wiki/pages", None, &page).await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (401, "unauthenticated".into())
    );

    // fc has wiki-edit.
    let created = create_ok(&w, page.clone()).await;
    assert_eq!(created["status"], "draft");
    assert_eq!(created["created_by"]["name"], "FC");
    let id = created["id"].as_i64().unwrap();

    // Sections and deletes need wiki-admin (fc+).
    let res = w
        .app
        .post_json(
            "/api/wiki/sections",
            Some(&w.fc),
            &json!({"key": "x", "label": "X"}),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (401, "access_denied".into())
    );
    let res = w
        .app
        .send_json(
            Method::DELETE,
            &format!("/api/wiki/pages/{id}"),
            Some(&w.fc),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 401);
    let res = w
        .app
        .send_json(
            Method::DELETE,
            &format!("/api/wiki/pages/{id}"),
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let res = w
        .app
        .get(&format!("/api/wiki/pages/{id}"), Some(&w.fcplus))
        .await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (404, "not_found".into())
    );

    // Restore (wiki-admin), listed in the trash first.
    let res = w.app.get("/api/wiki/deleted", Some(&w.fcplus)).await;
    assert_eq!(res.json::<Value>()["pages"][0]["id"], id);
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{id}/restore"),
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());

    // A taken path is a conflict; bad JSON is a JSON 422.
    let res = create(&w, &w.fc, page).await;
    assert_eq!((res.status.as_u16(), error(&res)), (409, "conflict".into()));
    let res = w
        .app
        .request(
            TestApp::build(Method::POST, "/api/wiki/pages", Some(&w.fc))
                .body(Body::from("{nope"))
                .unwrap(),
        )
        .await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (422, "invalid_json".into())
    );
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "Bad Path", "title": "A", "section": "start"}),
    )
    .await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (400, "bad_request".into())
    );
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "b", "title": "B", "section": "nope"}),
    )
    .await;
    assert_eq!(res.status, 400);
    assert!(res.text().contains("unknown section"));

    // whoami lists the new keys.
    let me: Value = w.app.get("/api/auth/whoami", Some(&w.fc)).await.json();
    let access: Vec<&str> = me["access"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(access.contains(&"wiki-edit") && !access.contains(&"wiki-admin"));
    let me: Value = w.app.get("/api/auth/whoami", Some(&w.admin)).await.json();
    assert!(
        me["access"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "wiki-admin")
    );
}

#[tokio::test]
async fn visibility_and_index() {
    let Some(w) = setup().await else { return };
    let public = create_ok(
        &w,
        json!({"slug": "guides/newbro", "title": "New pilots", "section": "start",
               "status": "published", "body": "See [FC](/wiki/fc/rules)."}),
    )
    .await;
    create_ok(
        &w,
        json!({"slug": "guides/members", "title": "Members", "section": "start",
               "status": "published", "visibility": "login", "position": 1}),
    )
    .await;
    create_ok(
        &w,
        json!({"slug": "fc/rules", "title": "FC rules", "section": "fc", "status": "published",
               "visibility": "permission", "permission": "fleet-view",
               "body": "Back to [newbros](/wiki/guides/newbro)."}),
    )
    .await;
    create_ok(
        &w,
        json!({"slug": "guides/draft", "title": "Draft", "section": "start"}),
    )
    .await;

    // Anonymous: only the public page; numbering closes up.
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(index["can_edit"], false);
    assert_eq!(index["sections"].as_array().unwrap().len(), 1);
    assert_eq!(index["sections"][0]["pages"][0]["slug"], "guides/newbro");
    assert_eq!(index["sections"][0]["pages"][0]["num"], "1.1");

    // A logged-in pilot sees the login page first (position 1).
    let index: Value = w.app.get("/api/wiki", Some(&w.user)).await.json();
    let nums: Vec<(String, String)> = index["sections"][0]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["num"].as_str().unwrap().into(),
                p["slug"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        nums,
        vec![
            ("1.1".into(), "guides/members".into()),
            ("1.2".into(), "guides/newbro".into())
        ]
    );
    // A trainee (fleet-view) sees the FC section too.
    let index: Value = w.app.get("/api/wiki", Some(&w.trainee)).await.json();
    assert_eq!(index["sections"][1]["num"], "2");
    assert_eq!(index["sections"][1]["pages"][0]["num"], "2.1");
    // Editors see drafts only on request.
    let index: Value = w.app.get("/api/wiki?drafts=true", Some(&w.fc)).await.json();
    assert_eq!(index["sections"][0]["pages"].as_array().unwrap().len(), 3);
    let index: Value = w
        .app
        .get("/api/wiki?drafts=true", Some(&w.user))
        .await
        .json();
    assert_eq!(index["sections"][0]["pages"].as_array().unwrap().len(), 2);

    // Page reads respect visibility; backlinks only from pages the reader may see.
    let res = w.app.get("/api/wiki/page?slug=fc/rules", None).await;
    assert_eq!(res.status, 404);
    let res = w
        .app
        .get("/api/wiki/page?slug=fc/rules", Some(&w.user))
        .await;
    assert_eq!(res.status, 404);
    let res = w
        .app
        .get("/api/wiki/page?slug=fc/rules", Some(&w.trainee))
        .await;
    assert_eq!(res.status, 200);
    let res = w
        .app
        .get("/api/wiki/page?slug=guides/draft", Some(&w.user))
        .await;
    assert_eq!(res.status, 404);
    let res = w
        .app
        .get("/api/wiki/page?slug=guides/draft", Some(&w.fc))
        .await;
    assert_eq!(res.status, 200);

    let res = w.app.get("/api/wiki/page?slug=guides/newbro", None).await;
    assert_eq!(res.status, 200);
    let page: Value = res.json();
    assert_eq!(
        res.header("etag").unwrap(),
        format!("\"{}\"", page["revision_id"])
    );
    assert_eq!(page["backlinks"], json!([]));
    assert_eq!(
        page["section"],
        json!({"key": "start", "label": "Start here"})
    );
    assert_eq!(page["read_minutes"], 1);
    let page: Value = w
        .app
        .get("/api/wiki/page?slug=guides/newbro", Some(&w.trainee))
        .await
        .json();
    assert_eq!(
        page["backlinks"],
        json!([{"slug": "fc/rules", "title": "FC rules"}])
    );
    assert_eq!(page["num"], "1.2");

    // Restricting to a key you don't hold is refused; an unknown key is a bad request.
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "x", "title": "X", "section": "start", "visibility": "permission",
               "permission": "commanders-manage:admin"}),
    )
    .await;
    assert_eq!(res.status, 401, "{}", res.text());
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "x", "title": "X", "section": "start", "visibility": "permission",
               "permission": "no-such-key"}),
    )
    .await;
    assert_eq!(res.status, 400);

    // Visibility of another author's page needs wiki-admin.
    let id = public["id"].as_i64().unwrap();
    let other = w.app.create_account(9006, "Other FC", Some("fc")).await;
    let res = put(&w, &other, id, json!({"visibility": "login"})).await;
    assert_eq!(res.status, 401, "{}", res.text());
    let res = put(&w, &w.fcplus, id, json!({"visibility": "login"})).await;
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(
        w.app
            .get("/api/wiki/page?slug=guides/newbro", None)
            .await
            .status,
        404
    );

    // Unpublish / publish.
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{id}/unpublish"),
            Some(&w.fc),
            &json!({}),
        )
        .await;
    assert_eq!(res.json::<Value>()["status"], "draft");
    assert_eq!(
        w.app
            .get("/api/wiki/page?slug=guides/newbro", Some(&w.user))
            .await
            .status,
        404
    );
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{id}/publish"),
            Some(&w.fc),
            &json!({}),
        )
        .await;
    let page: Value = res.json();
    assert_eq!(page["status"], "published");
    assert!(page["published_at"].as_i64().is_some());
}

#[tokio::test]
async fn concurrency_revisions_diff_and_revert() {
    let Some(w) = setup().await else { return };
    let page = create_ok(
        &w,
        json!({"slug": "p", "title": "P", "section": "start", "body": "one\ntwo\n"}),
    )
    .await;
    let id = page["id"].as_i64().unwrap();
    let r1 = page["revision_id"].as_i64().unwrap();

    // Content changes need a base revision.
    let res = put(&w, &w.fc, id, json!({"body": "x"})).await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (428, "precondition_required".into())
    );

    let res = put(
        &w,
        &w.fc,
        id,
        json!({"base_revision": r1, "body": "one\nTWO\nthree\n", "message": "Edit"}),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let r2 = res.json::<Value>()["revision_id"].as_i64().unwrap();
    assert_ne!(r1, r2);

    // A second editor still on r1 gets a conflict with the current revision.
    let res = put(
        &w,
        &w.fcplus,
        id,
        json!({"base_revision": r1, "body": "mine"}),
    )
    .await;
    assert_eq!(res.status, 409);
    let body: Value = res.json();
    assert_eq!(body["error"], "conflict");
    assert_eq!(body["current_revision"], r2);

    // If-Match works as the base revision.
    let res = w
        .app
        .request(
            TestApp::build(
                Method::PUT,
                &format!("/api/wiki/pages/{id}"),
                Some(&w.fcplus),
            )
            .header(header::IF_MATCH, format!("\"{r2}\""))
            .body(Body::from(json!({"title": "P2"}).to_string()))
            .unwrap(),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let r3 = res.json::<Value>()["revision_id"].as_i64().unwrap();
    // Saving the same content creates no revision.
    let res = put(&w, &w.fc, id, json!({"base_revision": r3, "title": "P2"})).await;
    assert_eq!(res.json::<Value>()["revision_id"], r3);

    let revs: Value = w
        .app
        .get(&format!("/api/wiki/pages/{id}/revisions"), Some(&w.fc))
        .await
        .json();
    let revs = revs["revisions"].as_array().unwrap();
    assert_eq!(revs.len(), 3);
    assert_eq!(revs[0]["id"], r3);
    assert_eq!(revs[0]["current"], true);
    assert_eq!(revs[0]["author"]["name"], "Senior FC");
    assert_eq!(revs[1]["message"], "Edit");
    assert_eq!(revs[2]["message"], "Created");
    // Revisions are for editors.
    let res = w
        .app
        .get(&format!("/api/wiki/pages/{id}/revisions"), Some(&w.user))
        .await;
    assert_eq!(res.status, 401);

    let rev: Value = w
        .app
        .get(&format!("/api/wiki/pages/{id}/revisions/{r1}"), Some(&w.fc))
        .await
        .json();
    assert_eq!(rev["body"], "one\ntwo\n");
    assert_eq!(rev["current"], false);

    let diff: Value = w
        .app
        .get(&format!("/api/wiki/pages/{id}/diff?from={r1}"), Some(&w.fc))
        .await
        .json();
    assert_eq!(diff["to"], r3);
    assert_eq!(
        (diff["insertions"].as_i64(), diff["deletions"].as_i64()),
        (Some(2), Some(1))
    );
    assert_eq!(diff["from_title"], "P");
    assert_eq!(diff["to_title"], "P2");
    assert!(
        diff["unified"]
            .as_str()
            .unwrap()
            .contains("-two\n+TWO\n+three\n")
    );

    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{id}/revert"),
            Some(&w.fc),
            &json!({"revision": r1}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let page: Value = res.json();
    assert_eq!(page["body"], "one\ntwo\n");
    assert_eq!(page["title"], "P");
    let revs: Value = w
        .app
        .get(&format!("/api/wiki/pages/{id}/revisions"), Some(&w.fc))
        .await
        .json();
    assert_eq!(
        revs["revisions"][0]["message"],
        format!("Reverted to revision {r1}")
    );
    // A stale base on revert is a conflict too.
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{id}/revert"),
            Some(&w.fc),
            &json!({"revision": r2, "base_revision": r1}),
        )
        .await;
    assert_eq!(res.status, 409);
}

#[tokio::test]
async fn directives_are_validated() {
    let Some(w) = setup().await else { return };
    let body =
        ":::callout{kind=loud}\nx\n:::\n\n:::tabs\n:::\n\n::::steps\n:::step\nx\n:::\n::::\n";
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "p", "title": "P", "section": "start", "body": body}),
    )
    .await;
    assert_eq!(res.status, 400);
    let err: Value = res.json();
    assert_eq!(err["error"], "invalid_markdown");
    let problems: Vec<(i64, &str)> = err["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["line"].as_i64().unwrap(), p["code"].as_str().unwrap()))
        .collect();
    assert_eq!(
        problems,
        vec![
            (1, "invalid_attribute"),
            (5, "unknown_directive"),
            (9, "missing_attribute")
        ]
    );

    let res = w
        .app
        .post_json(
            "/api/wiki/validate",
            Some(&w.fc),
            &json!({"body": "## A\n::fit"}),
        )
        .await;
    let v: Value = res.json();
    assert_eq!(v["valid"], false);
    assert_eq!(v["problems"][0]["code"], "missing_attribute");
    assert_eq!(v["headings"][0]["id"], "a");
    let res = w
        .app
        .post_json("/api/wiki/validate", Some(&w.user), &json!({"body": ""}))
        .await;
    assert_eq!(res.status, 401);

    // Without fit data loaded, any doctrine fit name is accepted.
    let res = create(
        &w,
        &w.fc,
        json!({"slug": "p", "title": "P", "section": "start", "body": "::fit{name=\"Anything\"}"}),
    )
    .await;
    assert_eq!(res.status, 201, "{}", res.text());
}

/// With the doctrine fits loaded, `::fit{name=…}` must name one.
#[tokio::test]
async fn fit_names_are_checked_against_the_doctrine() {
    let path = std::env::var_os("WL_SDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
        });
    if !path.exists() {
        eprintln!("skipping: static data file {} not found", path.display());
        return;
    }
    let eve = Arc::new(wl_eve_data::EveData::load(&path).await.unwrap());
    let Some(app) = TestApp::with_services(move |state, _, dir| {
        wl_fitting::example_data::seed(dir, true).unwrap();
        let store = wl_fitting::DataStore::open(eve.clone(), dir).unwrap();
        state.with_eve_data(eve).with_data_store(Arc::new(store))
    })
    .await
    else {
        return;
    };
    let fc = app.create_account(1, "FC", Some("fc+")).await;
    app.post_json(
        "/api/wiki/sections",
        Some(&fc),
        &json!({"key": "s", "label": "S"}),
    )
    .await;
    let res = app
        .post_json(
            "/api/wiki/validate",
            Some(&fc),
            &json!({"body": "::fit{name=\"ARMOR_NESTOR\"}\n::fit{name=\"HQ Nestr\"}"}),
        )
        .await;
    let v: Value = res.json();
    assert_eq!(v["problems"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["problems"][0]["code"], "unknown_fit");
    assert_eq!(v["problems"][0]["line"], 2);
    let res = app
        .post_json(
            "/api/wiki/pages",
            Some(&fc),
            &json!({"slug": "p", "title": "P", "section": "s", "body": "::fit{name=\"HQ Nestr\"}"}),
        )
        .await;
    assert_eq!(res.status, 400);
    assert_eq!(res.json::<Value>()["problems"][0]["code"], "unknown_fit");
}

#[tokio::test]
async fn sections_and_moves() {
    let Some(w) = setup().await else { return };
    let a = create_ok(
        &w,
        json!({"slug": "a", "title": "A", "section": "start", "status": "published"}),
    )
    .await;
    let b = create_ok(
        &w,
        json!({"slug": "b", "title": "B", "section": "start", "status": "published"}),
    )
    .await;
    let c = create_ok(&w, json!({"slug": "c", "title": "C", "section": "start", "status": "published", "position": 1})).await;
    let order = |index: &Value, s: usize| -> Vec<String> {
        index["sections"][s]["pages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["slug"].as_str().unwrap().to_string())
            .collect()
    };
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(order(&index, 0), vec!["c", "a", "b"]);

    // Reorder within the section, then move to another section with a new path.
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{}/move", b["id"]),
            Some(&w.fc),
            &json!({"position": 1}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(order(&index, 0), vec!["b", "c", "a"]);
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{}/move", c["id"]),
            Some(&w.fc),
            &json!({"section": "fc", "slug": "fc/c"}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(order(&index, 0), vec!["b", "a"]);
    assert_eq!(order(&index, 1), vec!["fc/c"]);
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{}/move", a["id"]),
            Some(&w.fc),
            &json!({"slug": "b"}),
        )
        .await;
    assert_eq!(res.status, 409);

    // Section order, rename, unlist, delete.
    let res = w
        .app
        .post_json(
            "/api/wiki/sections/order",
            Some(&w.fcplus),
            &json!({"keys": ["fc", "start"]}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(index["sections"][0]["key"], "fc");
    let res = w
        .app
        .post_json(
            "/api/wiki/sections/order",
            Some(&w.fcplus),
            &json!({"keys": ["fc"]}),
        )
        .await;
    assert_eq!(res.status, 400);
    let res = w
        .app
        .send_json(
            Method::PUT,
            "/api/wiki/sections/fc",
            Some(&w.fcplus),
            &json!({"label": "Commanders", "listed": false}),
        )
        .await;
    assert_eq!(res.json::<Value>()["label"], "Commanders");
    let index: Value = w.app.get("/api/wiki", None).await.json();
    assert_eq!(index["sections"].as_array().unwrap().len(), 1);
    // Unlisted pages: no number, but the section listing has them.
    let listing: Value = w
        .app
        .get("/api/wiki/sections/fc/pages?body=true", None)
        .await
        .json();
    assert_eq!(listing["pages"][0]["slug"], "fc/c");
    assert_eq!(listing["pages"][0]["num"], Value::Null);
    assert_eq!(listing["pages"][0]["body"], "");
    let res = w
        .app
        .send_json(
            Method::DELETE,
            "/api/wiki/sections/fc",
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!((res.status.as_u16(), error(&res)), (409, "conflict".into()));
    let res = w
        .app
        .send_json(
            Method::DELETE,
            &format!("/api/wiki/pages/{}", c["id"]),
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200);
    let res = w
        .app
        .send_json(
            Method::DELETE,
            "/api/wiki/sections/fc",
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    // The deleted page's section is gone: restoring needs another one.
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{}/restore", c["id"]),
            Some(&w.fcplus),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 409);
    let res = w
        .app
        .post_json(
            &format!("/api/wiki/pages/{}/restore", c["id"]),
            Some(&w.fcplus),
            &json!({"section": "start"}),
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text());
    let sections: Value = w.app.get("/api/wiki/sections", None).await.json();
    assert_eq!(sections["sections"][0]["pages"], 3);
}

#[tokio::test]
async fn search_ranks_and_filters() {
    let Some(w) = setup().await else { return };
    create_ok(&w, json!({"slug": "fc/training", "title": "FC training", "section": "fc", "status": "published",
        "summary": "Becoming an FC", "body": "## Shadow\nListen to fleets run by a trained FC."})).await;
    create_ok(
        &w,
        json!({"slug": "logi", "title": "Logistics", "section": "start", "status": "published",
        "body": "Logi pilots follow the FC's broadcasts and keep the fleet alive."}),
    )
    .await;
    create_ok(
        &w,
        json!({"slug": "secret", "title": "FC secrets", "section": "fc", "status": "published",
        "visibility": "permission", "permission": "fleet-view", "body": "FC only."}),
    )
    .await;
    create_ok(
        &w,
        json!({"slug": "draft", "title": "FC draft", "section": "fc"}),
    )
    .await;

    let res: Value = w.app.get("/api/wiki/search?q=fc", None).await.json();
    let slugs: Vec<&str> = res["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["fc/training", "logi"]);
    let hit = &res["hits"][1];
    let highlighted: Vec<&str> = hit["snippet"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["highlight"] == true)
        .map(|p| p["text"].as_str().unwrap())
        .collect();
    assert_eq!(highlighted, vec!["FC"]);
    assert_eq!(hit["section"]["label"], "Start here");

    let res: Value = w
        .app
        .get("/api/wiki/search?q=fc", Some(&w.trainee))
        .await
        .json();
    assert_eq!(res["hits"].as_array().unwrap().len(), 3);
    let res: Value = w.app.get("/api/wiki/search?q=fc", Some(&w.fc)).await.json();
    assert_eq!(
        res["hits"].as_array().unwrap().len(),
        4,
        "editors find drafts"
    );
    // Every word must match; word beginnings match while typing.
    let res: Value = w
        .app
        .get("/api/wiki/search?q=fc%20broadc", None)
        .await
        .json();
    assert_eq!(res["hits"][0]["slug"], "logi");
    assert_eq!(res["hits"].as_array().unwrap().len(), 1);
    let res: Value = w
        .app
        .get("/api/wiki/search?q=fc&limit=1", None)
        .await
        .json();
    assert_eq!(res["hits"].as_array().unwrap().len(), 1);

    // Edits show up in search right away.
    let page: Value = w
        .app
        .get("/api/wiki/page?slug=logi", Some(&w.fc))
        .await
        .json();
    put(
        &w,
        &w.fc,
        page["id"].as_i64().unwrap(),
        json!({"base_revision": page["revision_id"], "body": "Repairs."}),
    )
    .await;
    let res: Value = w.app.get("/api/wiki/search?q=repairs", None).await.json();
    assert_eq!(res["hits"][0]["slug"], "logi");
}

fn noisy_png(w: u32, h: u32) -> Vec<u8> {
    let mut seed = 12345u32;
    let img = image::RgbaImage::from_fn(w, h, |_, _| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let b = seed.to_be_bytes();
        image::Rgba([b[0], b[1], b[2], 255])
    });
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

async fn upload(w: &Wiki, login: Option<&Login>, name: &str, bytes: Vec<u8>) -> TestResponse {
    w.app
        .request(
            TestApp::build(
                Method::POST,
                &format!("/api/wiki/assets?filename={name}"),
                login,
            )
            .body(Body::from(bytes))
            .unwrap(),
        )
        .await
}

#[tokio::test]
async fn images() {
    let Some(w) = setup().await else { return };
    let res = upload(&w, Some(&w.user), "a.gif", b"GIF89a".to_vec()).await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (401, "access_denied".into())
    );
    let res = upload(&w, Some(&w.fc), "a.txt", b"x".to_vec()).await;
    assert_eq!(
        (res.status.as_u16(), error(&res)),
        (400, "bad_request".into())
    );

    let res = upload(&w, Some(&w.fc), "small.gif", b"GIF89a".to_vec()).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let small: Value = res.json();
    assert_eq!(small["filename"], "small.gif");
    assert_eq!(small["uploaded_by"]["name"], "FC");

    // A large opaque PNG is resized and stored as JPEG.
    let png = noisy_png(1700, 400);
    assert!(png.len() > 1024 * 1024, "{}", png.len());
    let res = upload(&w, Some(&w.fc), "shot.png", png).await;
    assert_eq!(res.status, 200, "{}", res.text());
    let shot: Value = res.json();
    assert_eq!(shot["filename"], "shot.jpg");
    assert_eq!(shot["content_type"], "image/jpeg");
    let url = shot["url"].as_str().unwrap();
    assert_eq!(url, format!("/api/wiki/assets/{}/shot.jpg", shot["id"]));

    // Anyone can load it; it is cached and sandboxed.
    let res = w.app.get(url, None).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.header("content-type"), Some("image/jpeg"));
    assert!(res.header("cache-control").unwrap().contains("immutable"));
    assert!(
        res.header("content-security-policy")
            .unwrap()
            .contains("sandbox")
    );
    let img = image::load_from_memory(&res.body).unwrap();
    assert!(img.width() <= 1600 && img.width() > 1500, "{}", img.width());
    assert!(
        w.app
            .data_dir
            .path()
            .join(format!("wiki/assets/{}.jpg", shot["id"]))
            .exists()
    );

    let list: Value = w.app.get("/api/wiki/assets", Some(&w.fc)).await.json();
    assert_eq!(list["assets"].as_array().unwrap().len(), 2);
    assert_eq!(list["assets"][0]["id"], shot["id"]);

    let res = w
        .app
        .send_json(
            Method::DELETE,
            &format!("/api/wiki/assets/{}", shot["id"]),
            Some(&w.fc),
            &json!({}),
        )
        .await;
    assert_eq!(res.status, 200);
    assert_eq!(w.app.get(url, None).await.status, 404);
    assert!(
        !w.app
            .data_dir
            .path()
            .join(format!("wiki/assets/{}.jpg", shot["id"]))
            .exists()
    );
}

#[tokio::test]
async fn imported_content_is_served() {
    let Some(w) = setup().await else { return };
    let plan = wl_wiki::import::plan(&fixtures());
    assert_eq!(plan.errors, vec![]);
    let report = w.app.state.wiki().import(&plan, false).await.unwrap();
    assert_eq!(report.errors, vec![]);
    assert_eq!(report.pages_created, 16);
    // `start` and `fc` existed already; `fc` moves to the order of its file.
    assert_eq!(
        (
            report.sections_updated,
            report.sections_unchanged,
            report.sections_created
        ),
        (1, 1, 4)
    );

    let page: Value = w
        .app
        .get("/api/wiki/page?slug=guides/newbro", None)
        .await
        .json();
    assert_eq!(page["for"], "All new pilots");
    assert_eq!(page["num"], "1.1");
    assert_eq!(page["headings"][0]["id"], "your-first-fleet-in-four-steps");
    assert_eq!(page["updated_by"], Value::Null);

    // Cheatsheets: for fleet-view, in one request.
    let res = w
        .app
        .get(
            "/api/wiki/sections/fc-cheatsheets/pages?body=true",
            Some(&w.user),
        )
        .await;
    assert_eq!(res.json::<Value>()["pages"], json!([]));
    let res: Value = w
        .app
        .get(
            "/api/wiki/sections/fc-cheatsheets/pages?body=true",
            Some(&w.trainee),
        )
        .await
        .json();
    let titles: Vec<&str> = res["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Target priority", "Invite priority"]);
    assert!(
        res["pages"][0]["body"]
            .as_str()
            .unwrap()
            .starts_with(":::callout")
    );
    assert_eq!(res["section"]["listed"], false);

    // An imported page can be edited in the app.
    let res = put(
        &w,
        &w.fc,
        page["id"].as_i64().unwrap(),
        json!({"base_revision": page["revision_id"], "summary": "Edited"}),
    )
    .await;
    assert_eq!(res.status, 200, "{}", res.text());
}

async fn next_event(sub: &mut wl_live::Subscription) -> Arc<wl_live::Event> {
    tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("timed out waiting for an event")
        .expect("stream ended")
}

#[tokio::test]
async fn changes_are_announced() {
    let Some(w) = setup().await else { return };
    let mut public = w.app.live().subscribe(vec![topics::WIKI.to_owned()]);
    let mut restricted = w
        .app
        .live()
        .subscribe(vec![topics::WIKI_RESTRICTED.to_owned()]);

    let page = create_ok(
        &w,
        json!({"slug": "p", "title": "P", "section": "start", "status": "published"}),
    )
    .await;
    let event = next_event(&mut public).await;
    assert_eq!(event.name, "wiki_update");
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(
        data,
        json!({"slug": "p", "section": "start", "change": "created"})
    );

    // A draft goes to the restricted topic only.
    create_ok(&w, json!({"slug": "d", "title": "D", "section": "start"})).await;
    let event = next_event(&mut restricted).await;
    assert_eq!(
        serde_json::from_str::<Value>(&event.data).unwrap()["slug"],
        "d"
    );

    // Making a public page restricted is announced publicly (readers must drop it).
    let id = page["id"].as_i64().unwrap();
    put(&w, &w.fc, id, json!({"visibility": "login"})).await;
    let event = next_event(&mut public).await;
    assert_eq!(
        serde_json::from_str::<Value>(&event.data).unwrap()["change"],
        "updated"
    );
    // ...and later changes to it are not.
    w.app
        .post_json(
            &format!("/api/wiki/pages/{id}/unpublish"),
            Some(&w.fc),
            &json!({}),
        )
        .await;
    let event = next_event(&mut restricted).await;
    assert_eq!(
        serde_json::from_str::<Value>(&event.data).unwrap()["change"],
        "unpublished"
    );

    // Sections are public.
    w.app
        .send_json(
            Method::PUT,
            "/api/wiki/sections/start",
            Some(&w.fcplus),
            &json!({"label": "Begin"}),
        )
        .await;
    let event = next_event(&mut public).await;
    let data: Value = serde_json::from_str(&event.data).unwrap();
    assert_eq!(
        data,
        json!({"slug": null, "section": "start", "change": "section"})
    );

    // Nothing else was queued on the public topic.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), public.next())
            .await
            .is_err()
    );

    // The streams carry the topics.
    let me = w.app.login(w.user.account_id);
    let account = wl_api::AuthenticatedAccount::from_session(
        w.app.pool(),
        wl_api::auth::Session {
            account_id: me.account_id,
            window_character_id: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    let streamed = wl_api::routes::sse::topics_for(&account);
    assert!(streamed.contains(&topics::WIKI.to_owned()));
    assert!(streamed.contains(&topics::WIKI_RESTRICTED.to_owned()));
}
