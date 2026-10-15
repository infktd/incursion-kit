# wl-api

The HTTP API (axum 0.8). It must stay compatible with the waitlist frontends already in use: same
paths, the same request and response JSON, status codes, plain-text bodies, cookie and live
events. The contract is the OpenAPI spec (`docs/openapi.json`, generated from the annotations
here) together with `docs/api.md`; intentional differences from the original waitlist are listed
in `docs/migrating.md`.

## Layout

```
src/
  lib.rs          router(state): routes + /api/docs + 404 fallbacks + middleware (request id, tracing, body limit)
  state.rs        AppState
  error.rs        ApiError (plain-text error mapping)
  openapi.rs      the OpenAPI document: tags, shared error responses, security scheme
  extract.rs      JsonBody<T> (JSON body without a Content-Type requirement)
  auth/           session cookie, AuthenticatedAccount extractor, roles → permission keys
  routes/         one module per legacy route file (auth, sse, waitlist/*, fleet, pilot, …)
tests/
  common/mod.rs   TestApp harness (TestDb + in-process router + fixtures)
```

## Writing a route

Each module in `routes/` exposes `pub fn routes() -> OpenApiRouter<AppState>`, and
`routes/mod.rs` merges them. Every handler carries a `#[utoipa::path]` annotation and is
registered with `utoipa_axum::routes!`, which adds it to the axum router and to the OpenAPI spec
in one step (`tests/openapi.rs` fails if a route bypasses it). Paths are full literals
(`/api/...`) because there is no mount prefix, with axum's `{param}` syntax. Handlers for the
same path with different methods go in one `routes!(a, b)`.

```rust
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(info))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
struct InfoQuery { character_id: i64 }

/// Pilot information                       // first line: the summary in the docs
///
/// What the endpoint does, for API users. // the rest: the description
#[utoipa::path(
    get,
    path = "/api/pilot/info",
    tag = tags::PILOT,
    security(("session" = ["pilot-view"])), // login + permission key; `[]` = any login,
                                            // `(), ("session" = [])` = login optional
    params(InfoQuery),
    responses((status = 200, description = "The pilot.", body = PilotInfo)),
)]
async fn info(
    State(state): State<AppState>,
    account: AuthenticatedAccount,          // 401 "401 Authorization Required" without a session
    Query(q): Query<InfoQuery>,
) -> ApiResult<Json<PilotInfo>> {
    account.authorize_character(state.db(), q.character_id, Some("pilot-view")).await?;
    let row: PilotRow = sqlx::query_as("SELECT … WHERE id = ?")
        .bind(q.character_id)
        .fetch_one(state.db())
        .await?;                            // sqlx errors → 500 "database error"
    Ok(Json(row.into()))
}
```

- **Spec.** Wire types derive `utoipa::ToSchema` (and `IntoParams` for query structs). Document
  the success responses and the endpoint's own errors; `openapi::spec` adds the shared 401, 404
  (malformed parameters), 422 (bad JSON) and 500 responses. Plain-text bodies are
  `body = String, content_type = "text/plain"`. Give generic handler names (`list`, `create`) an
  explicit unique `operation_id`. After changing the API, regenerate the committed spec:
  `cargo run -p waitlist -- openapi > docs/openapi.json`.
- **Responses.** Return `Json(...)` for JSON. Return `&'static str` for the legacy `"OK"` bodies,
  which are sent as text/plain.
- **Request bodies.** Use `JsonBody<T>` instead of `axum::Json<T>`. The legacy server accepted
  JSON without a `Content-Type` header, and `JsonBody` keeps that. Bad JSON gets a plain-text 422.
- **Queries.** Use runtime-checked `sqlx::query` / `query_as` / `query_scalar` with
  `#[derive(sqlx::FromRow)]` rows. Never use the `query!` macros, because the build must not need
  a database. Quote the `character` table name with backticks.
- **No unwraps on input.** Turn every failure into an `ApiError`.

## Errors

`ApiError` (`src/error.rs`) mirrors the legacy error mapping exactly. Every body is plain text,
except the structured data editors' validation errors:

| Variant | Status | Body |
|---|---|---|
| `Unauthenticated` (from the extractor) | 401 | `401 Authorization Required` |
| `AccessDenied` | 401 | `access denied` |
| `Esi(EsiError::MissingScope / NoToken)` | 401 | `ESI error: missing ESI scope` / `ESI error: no ESI token found` |
| `Esi(EsiError::WithMessage(code, msg))` | `code` | `ESI error: msg` |
| `BadRequest(msg)`, `Type`, `Fit`, `SkillPlan` | 400 | the message (`type error: …`, …) |
| `Forbidden(msg)` | 403 | `msg` |
| `NotFound(msg)` | 404 | `msg` |
| `UnprocessableEntity(msg)` | 422 | `msg` |
| `Invalid(report)` | 400 | JSON `ValidationReport` (`{valid, dry_run, errors: [{path, message, line?}], warnings}`); only the structured data editors (`routes/data/`) use it |
| `Database`, `Internal`, other ESI errors | 500 | `database error` / `internal error` / `ESI error: …` |

Server errors are logged with their details, and the details are never sent to the client.
`wl_core::EsiError` is the ESI client's error type, so `?` works on ESI calls. For errors from
other crates (fit parsing, type lookup, skill plans), add a `From` impl in `error.rs` that maps
them onto the matching variant.

Unknown paths and unsupported methods both return `404` with body `404 Not Found`, as the legacy
server did. It never sent 405.

## Authentication and permissions

- **Session.** The session is the `authToken` cookie: a Branca token over msgpack
  `{version, account_id, window_character_id}`, keyed by `app.token_secret`. It is byte-compatible
  with the legacy waitlist, so existing sessions keep working (`auth/session.rs`). To log someone
  in, return `SetSessionCookie { token: state.session_key().encode(session)?, secure: state.secure_cookies() }`.
- **Login required.** Take `AuthenticatedAccount` as a handler argument. `account.id` is the main
  character id. `account.access` holds the role's permission keys.
- **Login optional.** Take `Option<AuthenticatedAccount>`. A missing or invalid cookie gives
  `None`. A database error still returns 500.
- **Permission keys.** `account.require_access("fleet-view")?` checks a key and fails with 401
  `access denied`. `account.has_access(..)` returns a bool. Roles and their cumulative keys live in
  `auth/permissions.rs`. `commanders-manage:admin` is the "site admin" key.
- **Per-character endpoints.** Call
  `account.authorize_character(db, character_id, Some("override-key")).await?`. It passes when the
  character is the account's main or one of its alts, or when the account holds the override key.

## Live events (SSE)

Clients connect to `GET /api/sse/stream` (`routes/sse.rs`). The handler streams straight from the
in-process `wl_live::Hub`. The legacy server redirected to a separate SSE server instead.

To publish, use `state.live()`:

```rust
use wl_live::{events, topics};

// A waitlist changed: `waitlist_update` + the public `status_update` (routes/waitlist/notify.rs).
state.live().waitlist_changed(waitlist_id);
state.live().publish_json(topics::FLEET_COMP, events::COMP_UPDATE, &json!({"fleet_id": id}))
    .map_err(ApiError::internal)?;
state.live().publish(&topics::account(account_id), events::WAKEUP, text);   // plain-text data
```

- **Names are part of the contract.** Topic and event names must match the catalogue in
  `docs/api.md` ("Live events"), including the misspelled `announcments` topic and the
  `announcment;new` / `announcment;updated` events. Use the constants in `wl_live::topics` and
  `wl_live::events`.
- **Additive topics.** The `incursions` topic (`incursions_update`) and the `status` topic
  (`status_update`) are not in the legacy catalogue. They are served to anonymous clients
  (`incursions` on `GET /api/incursions/stream`, both on `GET /api/public/stream`) and not on the
  authenticated stream (`routes/incursions.rs`, `routes/public.rs`, `docs/incursions-api.md`).
- **Extensions.** New endpoints and fields beyond the legacy API say so in their doc comments
  and are listed in `docs/api.md` ("Extensions beyond the legacy API"). Legacy behaviour stays
  the default; an opt-in goes in a new optional parameter or field.
- **Publishing never blocks.** A subscriber whose buffer is full is disconnected. Its browser then
  reconnects and refetches.

## Application state

`AppState` currently holds the database pool (`state.db()`), the config (`state.config()`), the
live hub (`state.live()`) and the session key. Services from other crates (the ESI client, EVE
static data, fitting data) are added as `Option<T>` fields with a `with_*` builder and an accessor
that returns `Result<&T, ApiError>`. The module docs in `state.rs` describe the pattern. The binary
wires every service. Tests wire only the services they use.

- `state.content()`: locales, guides and guide images (`wl-content`), always present, rooted at
  `config.data.dir`. Its operations are blocking file I/O: run them through
  `routes::blocking(..)`.
- `state.wiki()`: the wiki service (`wl-wiki`), built per call over the pool, the content
  directory and the shared search cache; it publishes `wiki_update` live events. Wiki routes
  (`routes/wiki.rs`) answer JSON errors (`wl_wiki::WikiError`), not `ApiError`.
- `state.esi()`, `state.eve_data()`, `state.data_store()` (`wl-fitting`'s community data).
- Ban checks shared between routes live in `src/ban.rs`.

In tests, `TestApp::with_services(|state, db, data_dir| state.with_…)` wires services;
`app.data_dir` is a per-test temporary data directory (already used by `state.content()`).

## Testing

Integration tests live in `tests/*.rs` and use the harness in `tests/common/mod.rs`:

```rust
mod common;
use common::TestApp;

#[tokio::test]
async fn whoami_works() {
    let Some(app) = TestApp::new().await else { return };    // skipped without TEST_DATABASE_URL
    let me = app.create_account(1001, "Main", Some("fc")).await;  // character + role + session
    app.create_character(1002, "Alt").await;
    app.link_alt(1001, 1002).await;

    let res = app.get("/api/auth/whoami", Some(&me)).await;
    assert_eq!(res.status, 200);
    let body: serde_json::Value = res.json();
    // also: app.post_json(path, Some(&me), &json!({...})), app.request(custom), res.text(), res.header(..)
}
```

- **Database.** `TestApp::new()` creates a fresh, migrated database on `TEST_DATABASE_URL`, for
  example `mysql://root:root@127.0.0.1:3307` with the dev compose stack. The database is dropped
  when the test finishes. Requests run in-process through `tower::ServiceExt::oneshot`, so no
  socket is opened.
- **Live events.** `app.live()` gives you the hub, so you can publish events, or assert on what a
  handler published by subscribing first with `app.live().subscribe(vec![...])`.
- **Unit tests.** Pure logic is tested in `#[cfg(test)]` modules next to the code.
- **Running the tests:**
  ```sh
  TEST_DATABASE_URL=mysql://root:root@127.0.0.1:3307 cargo test -p wl-api
  ```
