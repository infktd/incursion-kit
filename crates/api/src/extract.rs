//! Request extractors with legacy-compatible behaviour.

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::ApiError;

/// A JSON request body that, like the legacy waitlist, does **not** require a
/// `Content-Type: application/json` header (old clients send JSON without it).
///
/// Rejections are plain text: 422 for JSON that doesn't match `T`, 400 if the body can't be read
/// (e.g. too large). Responses still use `axum::Json`.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::BadRequest(e.body_text()))?;
        serde_json::from_slice(&bytes)
            .map(JsonBody)
            .map_err(|e| ApiError::UnprocessableEntity(format!("invalid JSON body: {e}")))
    }
}

/// Path parameters. A value that doesn't parse (e.g. `/api/v2/bans/abc`) is answered with the
/// legacy `404 Not Found`: the legacy router treated it as "no route matches".
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<S, T> FromRequestParts<S> for Path<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(value)| Path(value))
            .map_err(|_| not_found())
    }
}

/// Query parameters. A missing or unparseable value (e.g. no `?character_id=`) is answered with
/// the legacy `404 Not Found`, as the legacy router did.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<S, T> FromRequestParts<S> for Query<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(value)| Query(value))
            .map_err(|_| not_found())
    }
}

fn not_found() -> ApiError {
    ApiError::NotFound("404 Not Found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use serde::Deserialize;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Input {
        code: String,
    }

    async fn extract(body: &'static str, content_type: Option<&str>) -> Result<Input, ApiError> {
        let mut builder = Request::builder().method("POST").uri("/");
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        let req = builder.body(Body::from(body)).unwrap();
        JsonBody::<Input>::from_request(req, &()).await.map(|j| j.0)
    }

    #[tokio::test]
    async fn accepts_json_without_content_type() {
        let expected = Input { code: "x".into() };
        assert_eq!(extract(r#"{"code":"x"}"#, None).await.unwrap(), expected);
        assert_eq!(
            extract(r#"{"code":"x"}"#, Some("text/plain"))
                .await
                .unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn bad_path_and_query_values_are_legacy_404s() {
        let (mut parts, _) = Request::builder()
            .uri("/x?character_id=abc")
            .body(Body::empty())
            .unwrap()
            .into_parts();
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Q {
            character_id: i64,
        }
        let err = Query::<Q>::from_request_parts(&mut parts, &())
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_bad_json_with_422() {
        let err = extract(r#"{"nope":1}"#, None).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
