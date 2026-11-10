use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::ApiError;

pub(crate) fn issue(source: Uuid) -> (String, Vec<u8>) {
    let secret: [u8; 32] = rand::random();
    let hash = Sha256::digest(secret).to_vec();
    (
        format!("spool_v1.{source}.{}", URL_SAFE_NO_PAD.encode(secret)),
        hash,
    )
}

pub(crate) async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let unauthorized = || ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized");
    let mut authorization = headers.get_all("authorization").iter();
    let value = authorization
        .next()
        .and_then(|value| value.to_str().ok())
        .ok_or_else(unauthorized)?;
    if authorization.next().is_some() {
        return Err(unauthorized());
    }
    let (scheme, token) = value.split_once(' ').ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(unauthorized());
    }
    let mut parts = token.split('.');
    if parts.next() != Some("spool_v1") {
        return Err(unauthorized());
    }
    let source = parts
        .next()
        .and_then(|id| id.parse::<Uuid>().ok())
        .ok_or_else(unauthorized)?;
    let secret = parts
        .next()
        .and_then(|secret| URL_SAFE_NO_PAD.decode(secret).ok())
        .filter(|secret| secret.len() == 32)
        .ok_or_else(unauthorized)?;
    if parts.next().is_some() {
        return Err(unauthorized());
    }
    let expected: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT token_hash FROM sources WHERE id = $1 AND revoked_at IS NULL")
            .bind(source)
            .fetch_optional(pool)
            .await
            .map_err(|error| ApiError::database("auth.lookup", error))?;
    let actual = Sha256::digest(&secret);
    if !expected
        .as_deref()
        .is_some_and(|expected| bool::from(expected.ct_eq(actual.as_slice())))
    {
        return Err(unauthorized());
    }
    sqlx::query("UPDATE sources SET last_seen_at = now() WHERE id = $1")
        .bind(source)
        .execute(pool)
        .await
        .map_err(|error| ApiError::database("auth.touch", error))?;
    Ok(source)
}

/// Any valid source token may read the whole archive.
pub(crate) async fn require(State(pool): State<PgPool>, request: Request, next: Next) -> Response {
    match authenticate(&pool, request.headers()).await {
        Ok(_) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}
