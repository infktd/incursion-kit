//! The OpenAPI spec covers every route, and `docs/openapi.json` is up to date.
//!
//! Routes are registered with `utoipa_axum::routes!`, which adds each handler to the router and
//! to the spec together. These tests make sure nothing bypasses it, that every documented
//! operation is actually routed, and that the annotations are complete. None needs a database.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;
use utoipa::openapi::path::{Operation, ParameterIn};
use utoipa::openapi::{OpenApi, RefOr};
use wl_api::AppState;
use wl_core::Config;
use wl_live::Hub;

/// Every `(method, path, operation)` of the spec.
fn operations(api: &OpenApi) -> Vec<(Method, String, Operation)> {
    let mut all = Vec::new();
    for (path, item) in &api.paths.paths {
        let methods = [
            (Method::GET, &item.get),
            (Method::POST, &item.post),
            (Method::PUT, &item.put),
            (Method::PATCH, &item.patch),
            (Method::DELETE, &item.delete),
            (Method::HEAD, &item.head),
            (Method::OPTIONS, &item.options),
        ];
        for (method, operation) in methods {
            if let Some(operation) = operation {
                all.push((method, path.clone(), operation.clone()));
            }
        }
    }
    all
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A route registered without `routes!` would be served but missing from the spec.
#[test]
fn every_route_is_registered_through_the_spec() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let forbidden = [
        ".route(",
        ".route_service(",
        ".nest(",
        ".nest_service(",
        "axum::routing",
        "Router::new()",
    ];
    let mut offenders = Vec::new();
    for file in files {
        // `lib.rs` serves the docs themselves (`/api/docs`, `/api/openapi.json`).
        if file == src.join("lib.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap();
        for (number, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            if forbidden.iter().any(|pattern| code.contains(pattern))
                && !code.contains("OpenApiRouter::new()")
            {
                offenders.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "register routes with `.routes(routes!(handler))` and a `#[utoipa::path]` annotation, \
         so they appear in the OpenAPI spec:\n{}",
        offenders.join("\n")
    );
}

/// The legacy-shaped router state, over a database that is never reached.
fn offline_state(data_dir: &Path) -> AppState {
    let config = Config::from_toml_str(&format!(
        r#"
[database]
url = "mysql://unused"
[app]
token_secret = "4b1d7f0c3e5a6b8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f"
[esi]
client_id = "id"
client_secret = "secret"
url = "http://waitlist.test/auth/cb"
[data]
dir = "{}"
"#,
        data_dir.display()
    ))
    .unwrap();
    let db = sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/none")
        .unwrap();
    AppState::new(config, db, Hub::new()).unwrap()
}

/// Every documented operation reaches a handler: the router's fallbacks are replaced by a
/// teapot, which no handler answers.
#[tokio::test]
async fn every_documented_operation_is_routed() {
    let data_dir = tempfile::tempdir().unwrap();
    let (router, api) = wl_api::routes::routes().split_for_parts();
    let teapot = || async { StatusCode::IM_A_TEAPOT };
    let router = router
        .fallback(teapot)
        .method_not_allowed_fallback(teapot)
        .with_state(offline_state(data_dir.path()));

    let mut unrouted = Vec::new();
    for (method, path, _) in operations(&api) {
        let uri = path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "1"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let request = Request::builder()
            .method(method.clone())
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let response =
            tokio::time::timeout(Duration::from_secs(10), router.clone().oneshot(request))
                .await
                .unwrap_or_else(|_| panic!("{method} {uri} timed out"))
                .unwrap();
        if response.status() == StatusCode::IM_A_TEAPOT {
            unrouted.push(format!("{method} {path}"));
        }
    }
    assert!(
        unrouted.is_empty(),
        "documented but not routed: {unrouted:?}"
    );
}

/// Annotations are complete: tags, summaries, a success response, path parameters, and
/// permission keys that exist.
#[test]
fn operations_are_fully_annotated() {
    let api = wl_api::openapi::openapi();
    let known_keys: BTreeSet<&str> = wl_api::auth::permissions::build_access_levels()["admin"]
        .iter()
        .copied()
        .collect();
    let tags: BTreeSet<String> = api
        .tags
        .iter()
        .flatten()
        .map(|tag| tag.name.clone())
        .collect();

    let mut problems = Vec::new();
    let mut operation_ids = BTreeSet::new();
    for (method, path, operation) in operations(&api) {
        let name = format!("{method} {path}");
        match operation.tags.as_deref() {
            Some([tag]) if tags.contains(tag) => {}
            other => problems.push(format!(
                "{name}: needs exactly one declared tag, has {other:?}"
            )),
        }
        if operation.summary.as_deref().is_none_or(str::is_empty) {
            problems.push(format!("{name}: no summary (doc comment)"));
        }
        if let Some(id) = &operation.operation_id
            && !operation_ids.insert(id.clone())
        {
            problems.push(format!("{name}: duplicate operation id {id}"));
        }
        let responses = &operation.responses.responses;
        if !responses
            .keys()
            .any(|status| status.starts_with('2') || status.starts_with('3'))
        {
            problems.push(format!("{name}: no success response"));
        }
        let path_params: BTreeSet<String> = operation
            .parameters
            .iter()
            .flatten()
            .filter_map(|param| match param {
                RefOr::T(param) if param.parameter_in == ParameterIn::Path => {
                    Some(param.name.clone())
                }
                _ => None,
            })
            .collect();
        let in_path: BTreeSet<String> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .map(String::from)
            .collect();
        if path_params != in_path {
            problems.push(format!(
                "{name}: path parameters {path_params:?} don't match the path"
            ));
        }
        let security = serde_json::to_value(&operation.security).unwrap();
        for requirement in security.as_array().into_iter().flatten() {
            for (scheme, scopes) in requirement.as_object().unwrap() {
                if scheme != wl_api::openapi::SESSION {
                    problems.push(format!("{name}: unknown security scheme {scheme}"));
                }
                for scope in scopes.as_array().unwrap() {
                    let key = scope.as_str().unwrap();
                    if !known_keys.contains(key) {
                        problems.push(format!("{name}: unknown permission key {key}"));
                    }
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// `docs/openapi.json` is what `waitlist openapi` prints. Regenerate it with
/// `cargo run -p waitlist -- openapi > docs/openapi.json` (or run this test with
/// `UPDATE_OPENAPI=1`).
#[test]
fn committed_spec_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json");
    let generated = wl_api::openapi::openapi_json();
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "docs/openapi.json is out of date: run `cargo run -p waitlist -- openapi > \
         docs/openapi.json` (or this test with UPDATE_OPENAPI=1)"
    );
}

#[test]
fn spec_is_openapi_3_1() {
    let json: serde_json::Value = serde_json::from_str(&wl_api::openapi::openapi_json()).unwrap();
    assert_eq!(json["openapi"], "3.1.0");
    assert_eq!(
        json["components"]["securitySchemes"]["session"]["name"],
        "authToken"
    );
}

/// The docs UI and the raw spec are served (outside the spec).
#[tokio::test]
async fn docs_are_served() {
    let data_dir = tempfile::tempdir().unwrap();
    let router = wl_api::router(offline_state(data_dir.path()));
    for (path, content_type) in [
        (wl_api::DOCS_PATH, "text/html"),
        (wl_api::OPENAPI_PATH, "application/json"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let header = response.headers()["content-type"].to_str().unwrap();
        assert!(header.starts_with(content_type), "{path}: {header}");
    }
}

/// Schema components are keyed by type name, and a second type with the same name silently
/// replaces the first in the spec. Every `ToSchema` type in the workspace needs a unique name
/// (rename with `#[schema(as = UniqueName)]`).
#[test]
fn schema_names_are_unique() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    let mut seen: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        let mut attributes = String::new();
        let mut depth = 0i32;
        for line in source.lines() {
            let trimmed = line.trim();
            if depth > 0 || trimmed.starts_with("#[") || trimmed.starts_with("//") {
                if !trimmed.starts_with("//") {
                    depth += trimmed.matches('[').count() as i32;
                    depth -= trimmed.matches(']').count() as i32;
                }
                attributes.push_str(trimmed);
                attributes.push('\n');
                continue;
            }
            let item = trimmed
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            let name = item
                .strip_prefix("struct ")
                .or_else(|| item.strip_prefix("enum "))
                .and_then(|rest| {
                    rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                });
            if let Some(name) = name
                && attributes.contains("ToSchema")
            {
                let schema_name = attributes
                    .split("as = ")
                    .nth(1)
                    .and_then(|rest| {
                        rest.split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
                            .next()
                    })
                    .map(|path| path.rsplit("::").next().unwrap_or(path).to_string())
                    .unwrap_or_else(|| name.to_string());
                seen.entry(schema_name)
                    .or_default()
                    .push(file.strip_prefix(&crates).unwrap().display().to_string());
            }
            attributes.clear();
        }
    }
    let clashes: Vec<String> = seen
        .iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(name, files)| format!("{name}: {}", files.join(", ")))
        .collect();
    assert!(
        clashes.is_empty(),
        "schema names used by more than one type:\n{}",
        clashes.join("\n")
    );
    assert!(seen.contains_key("SpawnView") && seen.contains_key("Character"));
}
