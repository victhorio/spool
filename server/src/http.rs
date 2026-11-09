use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{
        Path, Query, Request, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;
use sqlx::{FromRow, PgPool};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{auth, error::ApiError, snapshot, snapshot::MAX_BODY};

#[derive(Clone)]
struct App {
    pool: PgPool,
    uploads: Arc<Semaphore>,
}

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/sync/source", put(assign_source_alias))
        .route("/v1/sync/sessions", get(inventory))
        .route("/v1/sync/sessions/{id}/snapshot", post(upload))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found") })
        .method_not_allowed_fallback(|| async {
            ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        })
        .with_state(App {
            pool,
            uploads: Arc::new(Semaphore::new(2)),
        })
}

async fn health(State(app): State<App>) -> StatusCode {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&app.pool)
        .await
    {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAlias {
    alias: String,
}

async fn assign_source_alias(
    State(app): State<App>,
    request: Request,
) -> Result<Json<serde_json::Value>, ApiError> {
    let headers = request.headers();
    let source = auth::authenticate(&app.pool, headers).await?;
    if !headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        ));
    }
    if headers.get_all("content-encoding").iter().any(|value| {
        !value
            .to_str()
            .is_ok_and(|encoding| encoding.trim().eq_ignore_ascii_case("identity"))
    }) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
        ));
    }
    let body = tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), 1024))
        .await
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "request_timeout"))?
        .map_err(|error| {
            use std::error::Error;
            if error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
            {
                ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
            } else {
                ApiError::malformed()
            }
        })?;
    if body.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(ApiError::malformed());
    }
    let SourceAlias { alias } = serde_json::from_slice(&body).map_err(|_| ApiError::malformed())?;
    if !(1..=32).contains(&alias.len())
        || !alias.as_bytes()[0].is_ascii_lowercase()
        || !alias
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ApiError::malformed());
    }
    let assigned = sqlx::query("UPDATE sources SET alias = $2 WHERE id = $1 AND alias IS NULL")
        .bind(source)
        .bind(&alias)
        .execute(&app.pool)
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(|error| {
                error.is_unique_violation() && error.constraint() == Some("sources_alias_key")
            }) {
                ApiError::new(StatusCode::CONFLICT, "source_alias_conflict")
            } else {
                ApiError::database("source.alias.assign", error)
            }
        })?;
    if assigned.rows_affected() == 0 {
        let existing: String = sqlx::query_scalar("SELECT alias FROM sources WHERE id = $1")
            .bind(source)
            .fetch_one(&app.pool)
            .await
            .map_err(|error| ApiError::database("source.alias.lookup", error))?;
        if existing != alias {
            return Err(ApiError::new(StatusCode::CONFLICT, "source_alias_mismatch"));
        }
    }
    Ok(Json(json!({"alias": alias})))
}

#[derive(Default, Deserialize)]
struct InventoryQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(FromRow)]
struct InventoryRow {
    session_id: String,
    revision: i64,
    deleted: bool,
}

fn cursor(source: Uuid, after: &str) -> String {
    let mut bytes = Vec::with_capacity(17 + after.len());
    bytes.push(1);
    bytes.extend_from_slice(source.as_bytes());
    bytes.extend_from_slice(after.as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn read_cursor(source: Uuid, cursor: &str) -> Result<String, ApiError> {
    if cursor.len() > 1024 {
        return Err(ApiError::malformed());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::malformed())?;
    if bytes.len() < 18 || bytes[0] != 1 || &bytes[1..17] != source.as_bytes() {
        return Err(ApiError::malformed());
    }
    let after = String::from_utf8(bytes[17..].to_vec()).map_err(|_| ApiError::malformed())?;
    if !snapshot::valid_id(&after) {
        return Err(ApiError::malformed());
    }
    Ok(after)
}

async fn inventory(
    State(app): State<App>,
    headers: HeaderMap,
    query: Result<Query<InventoryQuery>, QueryRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let source = auth::authenticate(&app.pool, &headers).await?;
    let Query(query) = query.map_err(|_| ApiError::malformed())?;
    let limit = query.limit.unwrap_or(200);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::malformed());
    }
    let after = query
        .cursor
        .as_deref()
        .map(|value| read_cursor(source, value))
        .transpose()?
        .unwrap_or_default();
    let mut rows: Vec<InventoryRow> = sqlx::query_as(
        "SELECT session_id, revision, deleted_at IS NOT NULL AS deleted FROM sessions \
         WHERE source_id = $1 AND session_id > $2 ORDER BY session_id LIMIT $3",
    )
    .bind(source)
    .bind(after)
    .bind(i64::from(limit) + 1)
    .fetch_all(&app.pool)
    .await
    .map_err(|error| ApiError::database("inventory.fetch", error))?;
    let next_cursor = if rows.len() > usize::from(limit) {
        rows.pop();
        rows.last().map(|row| cursor(source, &row.session_id))
    } else {
        None
    };
    let sessions: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({"session_id": row.session_id, "revision": row.revision.to_string(), "deleted": row.deleted})
        })
        .collect();
    Ok(Json(
        json!({"sessions": sessions, "next_cursor": next_cursor}),
    ))
}

async fn upload(
    State(app): State<App>,
    id: Result<Path<String>, PathRejection>,
    request: Request,
) -> Result<Response, ApiError> {
    let headers = request.headers();
    let source = auth::authenticate(&app.pool, headers).await?;
    let Path(id) = id.map_err(|_| ApiError::malformed())?;
    let permit = app
        .uploads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        ));
    }
    if headers.get_all("content-encoding").iter().any(|value| {
        !value
            .to_str()
            .is_ok_and(|encoding| encoding.trim().eq_ignore_ascii_case("identity"))
    }) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
        ));
    }
    if headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
        ));
    }
    let body = tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), MAX_BODY),
    )
    .await
    .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "request_timeout"))?
    .map_err(|error| {
        use std::error::Error;
        if error
            .source()
            .is_some_and(|source| source.is::<http_body_util::LengthLimitError>())
        {
            ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
        } else {
            ApiError::malformed()
        }
    })?;
    let session_id = id.clone();
    let (_permit, prepared) = tokio::task::spawn_blocking(move || {
        (permit, snapshot::Snapshot::parse(&body, &session_id))
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
    let accepted = snapshot::accept(&app.pool, source, &id, prepared?).await?;
    Ok((
        if accepted.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"revision": accepted.revision.to_string(), "changed": accepted.changed})),
    )
        .into_response())
}
