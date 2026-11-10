//! Device login (after RFC 8628): a machine requests a code, the owner approves it
//! in the reader, and the machine then collects a new source token once.

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{StatusCode, header},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{auth, error::ApiError};

/// How long a code waits for approval, and an approval waits for collection.
const LIFETIME_SECONDS: i32 = 600;
const POLL_SECONDS: u64 = 5;
const MAX_PENDING: i64 = 5;
const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

pub(crate) fn valid_alias(alias: &str) -> bool {
    (1..=32).contains(&alias.len())
        && alias.as_bytes()[0].is_ascii_lowercase()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Unauthenticated: the device code is the machine's credential until collection.
pub(crate) fn machine_routes() -> Router<PgPool> {
    Router::new()
        .route("/v1/device/code", post(request_code))
        .route("/v1/device/token", post(collect_token))
        .layer(DefaultBodyLimit::max(1024))
}

/// Mounted only in the reader, behind Cloudflare Access.
pub(crate) fn reader_routes() -> Router<PgPool> {
    Router::new()
        .route("/api/device/{code}", get(show))
        .route("/api/device/{code}/approve", post(approve))
        .route("/api/device/{code}/deny", post(deny))
}

fn database(operation: &'static str) -> impl FnOnce(sqlx::Error) -> ApiError {
    move |error| ApiError::database(operation, error)
}

/// Device codes and tokens must not be cached by anything between here and the machine.
type Private = ([(header::HeaderName, &'static str); 1], Json<Value>);

fn private(body: Value) -> Private {
    ([(header::CACHE_CONTROL, "no-store")], Json(body))
}

fn failure(code: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeRequest {
    alias: String,
}

async fn request_code(
    State(pool): State<PgPool>,
    body: Result<Json<CodeRequest>, JsonRejection>,
) -> Result<Private, ApiError> {
    let Json(CodeRequest { alias }) = body.map_err(|_| ApiError::malformed())?;
    if !valid_alias(&alias) {
        return Err(ApiError::malformed());
    }
    let secret: [u8; 32] = rand::random();
    let mut tx = pool.begin().await.map_err(database("device.code"))?;
    // Serializes the pending-code limit.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('spool.device_logins'))")
        .execute(&mut *tx)
        .await
        .map_err(database("device.code"))?;
    sqlx::query("DELETE FROM device_logins WHERE created_at < now() - interval '1 day'")
        .execute(&mut *tx)
        .await
        .map_err(database("device.code"))?;
    let (taken, waiting, pending): (bool, bool, i64) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM sources WHERE alias = $1), \
         EXISTS (SELECT 1 FROM device_logins WHERE alias = $1 AND source_id IS NULL \
             AND expires_at > now() AND approved IS NOT false), \
         (SELECT count(*) FROM device_logins WHERE decided_at IS NULL AND expires_at > now())",
    )
    .bind(&alias)
    .fetch_one(&mut *tx)
    .await
    .map_err(database("device.code"))?;
    if taken {
        return Err(ApiError::new(StatusCode::CONFLICT, "source_alias_conflict"));
    }
    if waiting {
        return Err(ApiError::new(StatusCode::CONFLICT, "login_pending"));
    }
    if pending >= MAX_PENDING {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_pending",
        ));
    }
    // 20^8 codes: a collision is rare, and repeated collisions are not expected.
    for _ in 0..3 {
        let user_code: String = {
            let mut rng = rand::rng();
            (0..8)
                .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
                .collect()
        };
        let inserted = sqlx::query(
            "INSERT INTO device_logins (device_code_hash, user_code, alias, expires_at) \
             VALUES ($1, $2, $3, now() + make_interval(secs => $4)) \
             ON CONFLICT (user_code) DO NOTHING",
        )
        .bind(Sha256::digest(secret).as_slice())
        .bind(&user_code)
        .bind(&alias)
        .bind(LIFETIME_SECONDS)
        .execute(&mut *tx)
        .await
        .map_err(database("device.code"))?
        .rows_affected();
        if inserted == 1 {
            tx.commit().await.map_err(database("device.code"))?;
            return Ok(private(json!({
                "device_code": URL_SAFE_NO_PAD.encode(secret),
                "user_code": display(&user_code),
                "expires_in": LIFETIME_SECONDS,
                "interval": POLL_SECONDS,
            })));
        }
    }
    Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "try_again"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenRequest {
    device_code: String,
}

#[derive(FromRow)]
struct Pending {
    alias: String,
    expired: bool,
    approved: Option<bool>,
    source_id: Option<Uuid>,
}

async fn collect_token(
    State(pool): State<PgPool>,
    body: Result<Json<TokenRequest>, JsonRejection>,
) -> Result<Private, ApiError> {
    let Json(TokenRequest { device_code }) = body.map_err(|_| ApiError::malformed())?;
    let secret = URL_SAFE_NO_PAD
        .decode(device_code)
        .ok()
        .filter(|secret| secret.len() == 32)
        .ok_or_else(ApiError::malformed)?;
    let hash = Sha256::digest(&secret);
    let mut tx = pool.begin().await.map_err(database("device.token"))?;
    let login: Option<Pending> = sqlx::query_as(
        "SELECT alias, expires_at <= now() AS expired, approved, source_id \
         FROM device_logins WHERE device_code_hash = $1 FOR UPDATE",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *tx)
    .await
    .map_err(database("device.token"))?;
    let alias = match login {
        None
        | Some(Pending {
            source_id: Some(_), ..
        }) => return Err(failure("invalid_device_code")),
        Some(Pending {
            approved: Some(false),
            ..
        }) => return Err(failure("access_denied")),
        Some(Pending { expired: true, .. }) => return Err(failure("expired_token")),
        Some(Pending { approved: None, .. }) => return Err(failure("authorization_pending")),
        Some(Pending { alias, .. }) => alias,
    };
    let source_id = Uuid::new_v4();
    let (token, token_hash) = auth::issue(source_id);
    let created = sqlx::query(
        "INSERT INTO sources (id, label, token_hash, alias) VALUES ($1, $2, $3, $2) \
         ON CONFLICT (alias) DO NOTHING",
    )
    .bind(source_id)
    .bind(&alias)
    .bind(token_hash)
    .execute(&mut *tx)
    .await
    .map_err(database("device.token"))?
    .rows_affected();
    if created == 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, "source_alias_conflict"));
    }
    sqlx::query("UPDATE device_logins SET source_id = $2 WHERE device_code_hash = $1")
        .bind(hash.as_slice())
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .map_err(database("device.token"))?;
    tx.commit().await.map_err(database("device.token"))?;
    Ok(private(
        json!({ "source_id": source_id, "alias": alias, "token": token }),
    ))
}

/// Accepts the displayed form (`WDJB-MJHT`) and common retyping variations.
fn normalize(code: &str) -> Result<String, ApiError> {
    let code: String = code
        .chars()
        .filter(|character| !matches!(character, '-' | ' '))
        .map(|character| character.to_ascii_uppercase())
        .collect();
    if code.len() == 8 && code.bytes().all(|byte| ALPHABET.contains(&byte)) {
        Ok(code)
    } else {
        Err(ApiError::malformed())
    }
}

fn display(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

#[derive(FromRow)]
struct Login {
    user_code: String,
    alias: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    expired: bool,
    approved: Option<bool>,
    source_id: Option<Uuid>,
}

impl Login {
    fn json(self) -> Json<Value> {
        let status = match (self.source_id, self.approved, self.expired) {
            (Some(_), _, _) => "completed",
            (None, Some(false), _) => "denied",
            (None, _, true) => "expired",
            (None, Some(true), false) => "approved",
            (None, None, false) => "pending",
        };
        Json(json!({
            "user_code": display(&self.user_code),
            "alias": self.alias,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "status": status,
        }))
    }
}

const LOGIN: &str = "SELECT user_code, alias, created_at, expires_at, \
    expires_at <= now() AS expired, approved, source_id FROM device_logins";

async fn find(pool: &PgPool, code: &str) -> Result<Login, ApiError> {
    sqlx::query_as(&format!("{LOGIN} WHERE user_code = $1"))
        .bind(code)
        .fetch_optional(pool)
        .await
        .map_err(database("device.show"))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))
}

async fn show(
    State(pool): State<PgPool>,
    code: Result<Path<String>, PathRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(code) = code.map_err(|_| ApiError::malformed())?;
    Ok(find(&pool, &normalize(&code)?).await?.json())
}

/// Approval restarts the clock, giving the machine the full lifetime to collect.
/// Until it does, an approval can still be withdrawn by denying.
async fn decide(pool: &PgPool, code: &str, approved: bool) -> Result<Json<Value>, ApiError> {
    let code = normalize(code)?;
    let decided: Option<Login> = sqlx::query_as(&format!(
        "WITH decided AS (UPDATE device_logins SET decided_at = now(), approved = $2, \
         expires_at = CASE WHEN $2 THEN now() + make_interval(secs => $3) ELSE expires_at END \
         WHERE user_code = $1 AND source_id IS NULL AND expires_at > now() \
         AND (decided_at IS NULL OR (approved AND NOT $2)) RETURNING *) \
         {}",
        LOGIN.replace("device_logins", "decided")
    ))
    .bind(&code)
    .bind(approved)
    .bind(LIFETIME_SECONDS)
    .fetch_optional(pool)
    .await
    .map_err(database("device.decide"))?;
    match decided {
        Some(login) => Ok(login.json()),
        None => {
            find(pool, &code).await?;
            Err(ApiError::new(StatusCode::CONFLICT, "device_not_pending"))
        }
    }
}

async fn approve(
    State(pool): State<PgPool>,
    code: Result<Path<String>, PathRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(code) = code.map_err(|_| ApiError::malformed())?;
    decide(&pool, &code, true).await
}

async fn deny(
    State(pool): State<PgPool>,
    code: Result<Path<String>, PathRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(code) = code.map_err(|_| ApiError::malformed())?;
    decide(&pool, &code, false).await
}
