//! Request bodies, paths and query strings, checked where they enter the service.

use std::time::Duration;

use axum::{
    body::{Bytes, to_bytes},
    extract::{FromRequestParts, Request},
    http::{HeaderMap, StatusCode, header, request::Parts},
};
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// A path parameter; a malformed one is a malformed request.
pub(crate) struct Path<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Path<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let axum::extract::Path(value) = axum::extract::Path::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::malformed())?;
        Ok(Self(value))
    }
}

/// A query string; a malformed one is a malformed request.
pub(crate) struct Query<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Query<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        let axum::extract::Query(value) = axum::extract::Query::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::malformed())?;
        Ok(Self(value))
    }
}

pub(crate) fn require_json(headers: &HeaderMap) -> Result<(), ApiError> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if json {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        ))
    }
}

/// Reads an uncompressed body of at most `limit` bytes, which must arrive within `timeout`.
pub(crate) async fn body(
    request: Request,
    limit: usize,
    timeout: Duration,
) -> Result<Bytes, ApiError> {
    let headers = request.headers();
    if headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            !value
                .to_str()
                .is_ok_and(|encoding| encoding.trim().eq_ignore_ascii_case("identity"))
        })
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
        ));
    }
    if content_length(headers).is_some_and(|length| length > limit as u64) {
        return Err(ApiError::too_large());
    }
    tokio::time::timeout(timeout, to_bytes(request.into_body(), limit))
        .await
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "request_timeout"))?
        .map_err(|error| {
            use std::error::Error;
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                ApiError::too_large()
            } else {
                ApiError::malformed()
            }
        })
}

pub(crate) fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

/// A JSON object body of at most `limit` bytes.
pub(crate) async fn json<T: DeserializeOwned>(
    request: Request,
    limit: usize,
) -> Result<T, ApiError> {
    require_json(request.headers())?;
    object(&body(request, limit, Duration::from_secs(5)).await?)
}

/// Parses a JSON object. Serde would also accept an array for a struct.
pub(crate) fn object<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    if body.trim_ascii_start().first() != Some(&b'{') {
        return Err(ApiError::malformed());
    }
    serde_json::from_slice(body).map_err(|_| ApiError::malformed())
}

/// A revision as clients send it: a positive decimal string.
pub(crate) fn revision(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or_else(ApiError::malformed)
}

/// The words of a search query, all of which must match: 1 to 16 of them.
pub(crate) fn terms(query: &str) -> Result<Vec<String>, ApiError> {
    if query.len() > 512 || query.contains('\0') {
        return Err(ApiError::malformed());
    }
    let terms: Vec<_> = query.split_whitespace().map(str::to_owned).collect();
    if terms.is_empty() || terms.len() > 16 {
        return Err(ApiError::malformed());
    }
    Ok(terms)
}

/// The `ILIKE` pattern for text containing `term`.
pub(crate) fn pattern(term: &str) -> String {
    format!(
        "%{}%",
        term.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}
