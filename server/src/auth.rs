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

/// The machine a request came from, set by [`require`].
#[derive(Clone, Copy)]
pub(crate) struct Source(pub Uuid);

/// Machine routes require a source token before anything else is read. Any
/// valid token may read the whole archive.
pub(crate) async fn require(
    State(pool): State<PgPool>,
    mut request: Request,
    next: Next,
) -> Response {
    match authenticate(&pool, request.headers()).await {
        Ok(source) => {
            request.extensions_mut().insert(Source(source));
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Result<Uuid, ApiError> {
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
    // last_seen_at is kept to the minute, so most requests only read.
    let expected: Option<(Vec<u8>, bool)> = sqlx::query_as(
        "SELECT token_hash, last_seen_at IS NULL OR last_seen_at < now() - interval '1 minute' \
         FROM sources WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(source)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::database("auth.lookup"))?;
    let actual = Sha256::digest(&secret);
    let Some((_, stale)) =
        expected.filter(|(expected, _)| bool::from(expected.ct_eq(actual.as_slice())))
    else {
        return Err(unauthorized());
    };
    if stale {
        sqlx::query("UPDATE sources SET last_seen_at = now() WHERE id = $1")
            .bind(source)
            .execute(pool)
            .await
            .map_err(ApiError::database("auth.touch"))?;
    }
    Ok(source)
}
