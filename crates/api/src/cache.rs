//! Revalidated caching for public JSON reads whose data a live stream announces changes of
//! (the incursion tracker, the public waitlist status).
//!
//! [`revalidate`] is a middleware: a `200` response gets `Cache-Control: public, no-cache` and a
//! strong `ETag` (a hash of the body). A `GET`/`HEAD` whose `If-None-Match` lists that tag (or
//! `*`) gets an empty `304 Not Modified` instead. Browsers and proxies may store the response but
//! must ask again before reusing it, so a refetch after a stream event always sees the new data,
//! and an unchanged answer costs a `304`. Only for routes that return JSON (not event streams):
//! the body is buffered to hash it.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::ApiError;

/// The `Cache-Control` of a revalidated response.
pub const CACHE_CONTROL: &str = "public, no-cache";

/// The middleware (`.layer(axum::middleware::from_fn(cache::revalidate))`).
pub async fn revalidate(request: Request, next: Next) -> Response {
    let conditional = matches!(*request.method(), Method::GET | Method::HEAD);
    let if_none_match = if conditional {
        request
            .headers()
            .get_all(header::IF_NONE_MATCH)
            .iter()
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let response = next.run(request).await;
    if response.status() != StatusCode::OK {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(e) => return ApiError::internal(e).into_response(),
    };
    let etag = etag(&body);
    parts.headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    parts.headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("hex is a valid header value"),
    );
    if if_none_match_hits(&if_none_match, &etag) {
        return not_modified(&parts.headers);
    }
    Response::from_parts(parts, Body::from(body))
}

/// A strong entity tag for `body`: the first 128 bits of its SHA-256, in hex, quoted.
pub fn etag(body: &[u8]) -> String {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, body);
    format!("\"{}\"", hex::encode(&digest.as_ref()[..16]))
}

/// Whether an `If-None-Match` (every value of the header; each a comma-separated list) matches
/// `etag`: `*`, or the same tag by weak comparison (RFC 9110 §13.1.2), i.e. ignoring `W/`.
fn if_none_match_hits(values: &[HeaderValue], etag: &str) -> bool {
    values
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}

/// An empty `304` with the headers a `200` would have carried that matter to caches.
fn not_modified(headers: &HeaderMap) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    for name in [header::CACHE_CONTROL, header::ETAG, header::VARY] {
        for value in headers.get_all(&name) {
            response.headers_mut().append(&name, value.clone());
        }
    }
    response
}

#[cfg(test)]
mod tests {
    // The middleware itself is covered through the real routes (`tests/incursions.rs`,
    // `tests/waitlist_ext.rs`).
    use super::*;

    #[test]
    fn tags_are_strong_and_depend_on_the_body() {
        let tag = etag(b"{}");
        assert!(
            tag.starts_with('"') && tag.ends_with('"') && tag.len() == 34,
            "{tag}"
        );
        assert_eq!(tag, etag(b"{}"));
        assert_ne!(tag, etag(b"{ }"));
    }

    #[test]
    fn if_none_match_lists() {
        let tag = "\"abc\"";
        let hits = |v: &str| if_none_match_hits(&[HeaderValue::from_str(v).unwrap()], tag);
        assert!(hits("\"abc\""));
        assert!(hits("W/\"abc\""));
        assert!(hits("\"x\", \"abc\""));
        assert!(hits("*"));
        assert!(!hits("\"abcd\""));
        assert!(!if_none_match_hits(&[], tag));
    }

    #[test]
    fn not_modified_keeps_the_cache_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ETAG, HeaderValue::from_static("\"abc\""));
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL),
        );
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let res = not_modified(&headers);
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(res.headers()[header::ETAG], "\"abc\"");
        assert_eq!(res.headers()[header::CACHE_CONTROL], CACHE_CONTROL);
        assert!(res.headers().get(header::CONTENT_TYPE).is_none());
    }
}
