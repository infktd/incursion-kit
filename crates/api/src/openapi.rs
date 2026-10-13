//! The OpenAPI 3.1 description of the API, generated from the route annotations.
//!
//! Every handler carries a `#[utoipa::path(...)]` attribute and is registered with
//! `utoipa_axum::routes!`, which adds it to the axum router *and* to the spec in one step, so
//! the two cannot drift apart (`tests/openapi.rs` also checks that nothing bypasses `routes!`).
//! [`spec`] adds the parts that are the same for every operation: the error responses (all
//! plain text) and the session security scheme.
//!
//! Conventions for annotations (see also `crates/api/README.md`):
//!
//! - `tag = tags::…` groups operations in the docs UI.
//! - Login required: `security(("session" = []))`; with a permission key:
//!   `security(("session" = ["fleet-view"]))`. Login optional: `security((), ("session" = []))`.
//!   No `security` means public.
//! - Document the success responses and the specific errors of the endpoint (400/403/404 with
//!   their messages). The generic 401, 404 (malformed parameters), 422 (bad JSON) and 500
//!   responses are added by [`spec`] (422 only for JSON bodies), and bodies of binary media
//!   types (images, `application/octet-stream`) are described as binary strings.
//! - Plain-text bodies: `body = String, content_type = "text/plain"`.

use utoipa::PartialSchema;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, SchemaFormat, Type};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{Content, OpenApi, RefOr, Response, ResponseBuilder};

/// Name of the session security scheme (the `authToken` cookie).
pub const SESSION: &str = "session";

/// Operation tags, in the order the docs UI lists them.
pub mod tags {
    pub const AUTH: &str = "auth";
    pub const LIVE: &str = "live";
    pub const WAITLIST: &str = "waitlist";
    pub const FLEET: &str = "fleet";
    pub const PILOT: &str = "pilot";
    pub const HISTORY: &str = "history";
    pub const DOCTRINE: &str = "doctrine";
    pub const CONTENT: &str = "content";
    pub const COMMUNITY: &str = "community";
    pub const ADMIN: &str = "admin";
    pub const SRP: &str = "srp";
    pub const INCURSIONS: &str = "incursions";
    pub const HEALTH: &str = "health";
}

#[derive(utoipa::OpenApi)]
#[openapi(
    info(
        title = "incursion-kit waitlist API",
        description = include_str!("openapi.md"),
        license(name = "MIT", identifier = "MIT"),
    ),
    tags(
        (name = tags::AUTH, description = "Session, EVE SSO login and logout."),
        (name = tags::LIVE, description = "Server-sent event streams."),
        (name = tags::WAITLIST, description = "The waitlist: x-up, approve, invite, remove."),
        (name = tags::FLEET, description = "Fleet registration, members, composition and MOTD roles."),
        (name = tags::PILOT, description = "Pilot information, alts, skills and notes."),
        (name = tags::HISTORY, description = "Fleet, x-up and skill history; statistics."),
        (name = tags::DOCTRINE, description = "Doctrine fits, skill plans, modules, implants and categories."),
        (name = tags::CONTENT, description = "Guides, guide assets and locales."),
        (name = tags::COMMUNITY, description = "Commanders, bans, badges, announcements and search."),
        (name = tags::ADMIN, description = "Data files and guide assets (administrators)."),
        (name = tags::SRP, description = "Ship replacement program."),
        (name = tags::INCURSIONS, description = "The public incursion tracker."),
        (name = tags::HEALTH, description = "Health check."),
    ),
)]
struct ApiDoc;

/// The base document (info, tags) that the routes are added to.
pub fn base() -> OpenApi {
    <ApiDoc as utoipa::OpenApi>::openapi()
}

/// The complete spec of `routes` (the document [`crate::router`] serves).
pub fn spec(routes: OpenApi) -> OpenApi {
    let mut api = base();
    api.merge(routes);
    finalize(&mut api);
    inline_primitive_refs(api)
}

/// Fields typed with an alias of a primitive (`TypeId = i32`) come out as references to a
/// component named after the primitive (`#/components/schemas/i32`). Inline those.
fn inline_primitive_refs(api: OpenApi) -> OpenApi {
    const PRIMITIVES: [&str; 12] = [
        "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "String",
    ];
    fn walk(
        value: &mut serde_json::Value,
        primitives: &serde_json::Map<String, serde_json::Value>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                let target = map
                    .get("$ref")
                    .and_then(|r| r.as_str())
                    .and_then(|r| r.strip_prefix("#/components/schemas/"))
                    .and_then(|name| primitives.get(name))
                    .cloned();
                if let Some(serde_json::Value::Object(schema)) = target {
                    map.remove("$ref");
                    for (key, value) in schema {
                        map.entry(key).or_insert(value);
                    }
                }
                map.values_mut().for_each(|v| walk(v, primitives));
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| walk(v, primitives)),
            _ => {}
        }
    }
    let mut json = serde_json::to_value(&api).expect("the OpenAPI document serializes");
    let schemas = json
        .pointer_mut("/components/schemas")
        .and_then(|s| s.as_object_mut());
    let Some(schemas) = schemas else { return api };
    let mut primitives = serde_json::Map::new();
    for name in PRIMITIVES {
        if let Some(schema) = schemas.remove(name) {
            primitives.insert(name.to_string(), schema);
        }
    }
    if primitives.is_empty() {
        return api;
    }
    walk(&mut json, &primitives);
    serde_json::from_value(json).expect("the OpenAPI document deserializes")
}

/// The complete spec, without building a router state (`waitlist openapi`).
pub fn openapi() -> OpenApi {
    spec(crate::routes::routes().into_openapi())
}

/// The spec as pretty-printed JSON with a trailing newline (the format of `docs/openapi.json`).
pub fn openapi_json() -> String {
    let mut json = openapi()
        .to_pretty_json()
        .expect("the OpenAPI document serializes");
    json.push('\n');
    json
}

fn text(description: &str) -> RefOr<Response> {
    ResponseBuilder::new()
        .description(description)
        .content("text/plain", Content::new(Some(String::schema())))
        .build()
        .into()
}

/// Bodies of non-text media types (images, `application/octet-stream`) are bytes: describe them
/// as binary strings rather than the arrays of integers `Vec<u8>` maps to.
fn binary_schemas<M>(content: &mut M)
where
    for<'a> &'a mut M: IntoIterator<Item = (&'a String, &'a mut RefOr<Content>)>,
{
    for (media_type, content) in content {
        let RefOr::T(content) = content else { continue };
        let textual = media_type.starts_with("text/")
            || media_type.contains("json")
            || media_type == "image/svg+xml";
        if !textual {
            content.schema = Some(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
                    .into(),
            );
        }
    }
}

/// Adds the session security scheme and the error responses every operation shares.
fn finalize(api: &mut OpenApi) {
    let components = api.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
        SESSION,
        SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
            "authToken",
            "The session cookie set by `POST /api/auth/cb`. Scopes listed on an operation are \
             the permission keys it requires (see the roles table in `docs/api.md`).",
        ))),
    );

    let empty = SecurityRequirement::default();
    for item in api.paths.paths.values_mut() {
        let operations = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.patch,
            &mut item.head,
            &mut item.options,
        ];
        for operation in operations.into_iter().flatten() {
            let login = operation.security.as_ref().map(|requirements| {
                // Required unless one alternative is the empty requirement.
                !requirements.contains(&empty)
            });
            let has_params = operation
                .parameters
                .as_ref()
                .is_some_and(|params| !params.is_empty());
            let json_body = operation
                .request_body
                .as_ref()
                .is_some_and(|body| match body {
                    RefOr::T(body) => body.content.contains_key("application/json"),
                    RefOr::Ref(_) => true,
                });
            if let Some(RefOr::T(body)) = &mut operation.request_body {
                binary_schemas(&mut body.content);
            }
            for response in operation.responses.responses.values_mut() {
                if let RefOr::T(response) = response {
                    binary_schemas(&mut response.content);
                }
            }
            let responses = &mut operation.responses.responses;
            if login == Some(true) {
                responses.entry("401".into()).or_insert_with(|| {
                    text(
                        "`401 Authorization Required` without a valid session; `access denied` \
                         without the required permission key or access to the character.",
                    )
                });
            }
            if has_params {
                responses.entry("404".into()).or_insert_with(|| {
                    text("`404 Not Found` when a path or query parameter is missing or malformed.")
                });
            }
            if json_body {
                responses.entry("422".into()).or_insert_with(|| {
                    text("`invalid JSON body: …` when the body doesn't match the schema.")
                });
            }
            responses.entry("500".into()).or_insert_with(|| {
                text("`database error`, `internal error` or `ESI error: …` (details are logged).")
            });
        }
    }
}
