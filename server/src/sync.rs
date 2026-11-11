//! Capture: machines name themselves, list what the archive holds, and upload
//! session snapshots. See docs/ingestion.md.

use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{FromRow, PgPool};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    auth::Source,
    error::ApiError,
    page,
    reference::{valid_alias, valid_id},
    request::{self, Path, Query},
    snapshot::{self, MAX_BODY},
};

#[derive(Clone)]
struct Sync {
    pool: PgPool,
    /// Snapshots are parsed in memory, so only a few at a time.
    uploads: Arc<Semaphore>,
}

pub(crate) fn machine_routes(pool: PgPool) -> Router {
    Router::new()
        .route("/v1/sync/source", put(assign_source_alias))
        .route("/v1/sync/sessions", get(inventory))
        .route("/v1/sync/sessions/{id}/snapshot", post(upload))
        .with_state(Sync {
            pool,
            uploads: Arc::new(Semaphore::new(2)),
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAlias {
    alias: String,
}

async fn assign_source_alias(
    State(sync): State<Sync>,
    Extension(Source(source)): Extension<Source>,
    request: Request,
) -> Result<Json<serde_json::Value>, ApiError> {
    let SourceAlias { alias } = request::json(request, 1024).await?;
    if !valid_alias(&alias) {
        return Err(ApiError::malformed());
    }
    let assigned = sqlx::query("UPDATE sources SET alias = $2 WHERE id = $1 AND alias IS NULL")
        .bind(source)
        .bind(&alias)
        .execute(&sync.pool)
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(|error| {
                error.is_unique_violation() && error.constraint() == Some("sources_alias_key")
            }) {
                ApiError::new(StatusCode::CONFLICT, "source_alias_conflict")
            } else {
                ApiError::database("source.alias.assign")(error)
            }
        })?;
    if assigned.rows_affected() == 0 {
        let existing: String = sqlx::query_scalar("SELECT alias FROM sources WHERE id = $1")
            .bind(source)
            .fetch_one(&sync.pool)
            .await
            .map_err(ApiError::database("source.alias.lookup"))?;
        if existing != alias {
            return Err(ApiError::new(StatusCode::CONFLICT, "source_alias_mismatch"));
        }
    }
    Ok(Json(json!({"alias": alias})))
}

#[derive(Deserialize)]
struct InventoryQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}

/// Inventory pages resume after a session ID, for one source only.
#[derive(Serialize, Deserialize)]
struct InventoryCursor {
    source: Uuid,
    after: String,
}

#[derive(FromRow)]
struct InventoryRow {
    session_id: String,
    revision: i64,
    deleted: bool,
}

async fn inventory(
    State(sync): State<Sync>,
    Extension(Source(source)): Extension<Source>,
    Query(query): Query<InventoryQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let limit = page::limit(query.limit, 200, 500)?;
    let after = match query.cursor.as_deref().map(page::decode).transpose()? {
        Some(InventoryCursor {
            source: issued,
            after,
        }) if issued == source && valid_id(&after) => after,
        Some(_) => return Err(ApiError::malformed()),
        None => String::new(),
    };
    let mut rows: Vec<InventoryRow> = sqlx::query_as(
        "SELECT session_id, revision, deleted_at IS NOT NULL AS deleted FROM sessions \
         WHERE source_id = $1 AND session_id > $2 ORDER BY session_id LIMIT $3",
    )
    .bind(source)
    .bind(after)
    .bind(i64::from(limit) + 1)
    .fetch_all(&sync.pool)
    .await
    .map_err(ApiError::database("inventory.fetch"))?;
    let next_cursor = page::last(&mut rows, limit).map(|row| {
        page::encode(&InventoryCursor {
            source,
            after: row.session_id.clone(),
        })
    });
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
    State(sync): State<Sync>,
    Extension(Source(source)): Extension<Source>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let permit = sync
        .uploads
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
    request::require_json(request.headers())?;
    let body = request::body(request, MAX_BODY, Duration::from_secs(30)).await?;
    let session_id = id.clone();
    let (_permit, prepared) = tokio::task::spawn_blocking(move || {
        (permit, snapshot::Snapshot::parse(&body, &session_id))
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable"))?;
    let accepted = snapshot::accept(&sync.pool, source, &id, prepared?).await?;
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
