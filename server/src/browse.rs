//! The reader's session list and transcripts.

use std::collections::BTreeMap;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{
    consistent_read,
    delivery::{self, Status},
    error::ApiError,
    page,
    reference::{self, LIVE_SESSIONS, Named, Reference, valid_id},
    request::{Path, Query},
};

/// Mounted only in the reader, behind Cloudflare Access; reads only.
pub(crate) fn reader_routes() -> Router<PgPool> {
    Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{*reference}", get(session))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
struct SessionQuery {
    cursor: Option<String>,
    limit: Option<u16>,
    q: Option<String>,
    source: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
enum Cursor {
    Sessions {
        activity: i64,
        source_id: Uuid,
        session_id: String,
    },
    /// Messages before `position`: the reader opens a session at its end.
    Messages {
        source_id: Uuid,
        session_id: String,
        revision: i64,
        position: i32,
    },
}

#[derive(FromRow)]
struct SessionRow {
    source_id: Uuid,
    source_label: String,
    alias: Option<String>,
    session_id: String,
    shared: bool,
    revision: i64,
    info: String,
    received_at: DateTime<Utc>,
    status: Status,
}

impl Named for SessionRow {
    fn name(&self) -> (Uuid, Option<&str>, &str) {
        (self.source_id, self.alias.as_deref(), &self.session_id)
    }
}

const SESSION_COLUMNS: &str = concat!(
    "SELECT s.source_id, sources.label AS source_label, sources.alias, s.session_id, ",
    reference::shared!("s"),
    " AS shared, s.revision, s.info::text AS info, s.received_at, ",
    delivery::status!(),
    " AS status"
);

#[derive(Serialize)]
struct Session {
    source_id: Uuid,
    source_label: String,
    session_id: String,
    session: String,
    revision: String,
    info: Box<RawValue>,
    received_at: DateTime<Utc>,
    status: Status,
}

fn raw(value: String) -> Result<Box<RawValue>, ApiError> {
    RawValue::from_string(value)
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "invalid_archive"))
}

impl SessionRow {
    fn response(self) -> Result<Session, ApiError> {
        Ok(Session {
            session: reference::format(
                self.source_id,
                self.alias.as_deref(),
                &self.session_id,
                self.shared,
            ),
            source_id: self.source_id,
            source_label: self.source_label,
            session_id: self.session_id,
            revision: self.revision.to_string(),
            info: raw(self.info)?,
            received_at: self.received_at,
            status: self.status,
        })
    }
}

#[derive(Default, Deserialize)]
struct Metadata<'a> {
    #[serde(borrow)]
    title: Option<&'a RawValue>,
    #[serde(borrow)]
    location: Option<&'a RawValue>,
    #[serde(borrow)]
    time: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Location {
    directory: Option<String>,
}

#[derive(Deserialize)]
struct Activity {
    updated: Option<f64>,
}

impl SessionRow {
    fn overview(&self) -> (i64, String) {
        let metadata = serde_json::from_str::<Metadata<'_>>(&self.info).unwrap_or_default();
        let activity = metadata
            .time
            .and_then(|time| serde_json::from_str::<Activity>(time.get()).ok())
            .and_then(|time| time.updated)
            .filter(|updated| updated.is_finite() && updated.abs() <= 8.64e15)
            .map(|updated| updated as i64)
            .unwrap_or_else(|| self.received_at.timestamp_millis());
        let title = metadata
            .title
            .and_then(|title| serde_json::from_str::<String>(title.get()).ok())
            .unwrap_or_default();
        let directory = metadata
            .location
            .and_then(|location| serde_json::from_str::<Location>(location.get()).ok())
            .and_then(|location| location.directory)
            .unwrap_or_default();
        (
            activity,
            format!("{title} {directory} {}", self.source_label).to_lowercase(),
        )
    }
}

#[derive(Serialize, FromRow)]
struct Source {
    source_id: Uuid,
    source_label: String,
}

#[derive(Serialize)]
struct Sessions {
    sessions: Vec<Session>,
    sources: Vec<Source>,
    next_cursor: Option<String>,
}

async fn sessions(
    State(pool): State<PgPool>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Sessions>, ApiError> {
    let limit = page::limit(query.limit, 50, 500)?;
    let after = match query.cursor.as_deref().map(page::decode).transpose()? {
        Some(Cursor::Sessions {
            activity,
            source_id,
            session_id,
        }) if valid_id(&session_id) => Some((activity, source_id, session_id)),
        None => None,
        _ => return Err(ApiError::malformed()),
    };
    let search = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    // PostgreSQL JSON extraction rejects accepted escaped NUL even in unrelated
    // fields. Rank the raw metadata here, retaining only one page of candidates.
    let sql =
        format!("{SESSION_COLUMNS}{LIVE_SESSIONS} AND ($1::uuid IS NULL OR s.source_id = $1)");
    let mut stream = sqlx::query_as::<_, SessionRow>(&sql)
        .bind(query.source)
        .fetch(&pool);
    let mut recent = BTreeMap::new();
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(ApiError::database("browse.sessions"))?
    {
        let (activity, text) = row.overview();
        let key = (activity, row.source_id, row.session_id.clone());
        if !text.contains(&search) || after.as_ref().is_some_and(|after| &key >= after) {
            continue;
        }
        recent.insert(key, row);
        if recent.len() > usize::from(limit) + 1 {
            recent.pop_first();
        }
    }
    drop(stream);
    let mut rows: Vec<_> = recent.into_iter().rev().collect();
    let next_cursor = page::last(&mut rows, limit).map(|((activity, source, session), _)| {
        page::encode(&Cursor::Sessions {
            activity: *activity,
            source_id: *source,
            session_id: session.clone(),
        })
    });
    let sources = sqlx::query_as(
        "SELECT id AS source_id, label AS source_label FROM sources \
         WHERE EXISTS (SELECT 1 FROM sessions WHERE source_id = sources.id AND deleted_at IS NULL) \
         ORDER BY label, id",
    )
    .fetch_all(&pool)
    .await
    .map_err(ApiError::database("browse.sources"))?;
    Ok(Json(Sessions {
        sessions: rows
            .into_iter()
            .map(|(_, row)| row.response())
            .collect::<Result<_, _>>()?,
        sources,
        next_cursor,
    }))
}

#[derive(Serialize)]
struct Transcript {
    #[serde(flatten)]
    session: Session,
    messages: Vec<Box<RawValue>>,
    /// The first message's index, as the agent tools number messages.
    first_index: i32,
    earlier_cursor: Option<String>,
}

#[derive(FromRow)]
struct MessageRow {
    position: i32,
    payload: String,
}

async fn session(
    State(pool): State<PgPool>,
    Path(reference): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Result<Json<Transcript>, ApiError> {
    let reference = Reference::parse(&reference)?;
    let limit = page::limit(query.limit, 100, 200)?;
    let before = match query.cursor.as_deref().map(page::decode).transpose()? {
        Some(Cursor::Messages {
            source_id,
            session_id,
            revision,
            position,
        }) if revision > 0 && position > 0 => Some((source_id, session_id, revision, position)),
        None => None,
        _ => return Err(ApiError::malformed()),
    };
    let mut tx = consistent_read(&pool).await?;
    let row: SessionRow = reference.resolve(SESSION_COLUMNS, &mut *tx).await?;
    let position = match before {
        Some((source, session, revision, position))
            if source == row.source_id && session == row.session_id =>
        {
            if revision != row.revision {
                return Err(ApiError::conflict(Some(row.revision)));
            }
            position
        }
        Some(_) => return Err(ApiError::malformed()),
        None => i32::MAX,
    };
    let (source, session) = (row.source_id, row.session_id.clone());
    let mut rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT position, payload::text AS payload FROM messages \
         WHERE source_id = $1 AND session_id = $2 AND position < $3 ORDER BY position DESC LIMIT $4",
    )
    .bind(source)
    .bind(&session)
    .bind(position)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(ApiError::database("browse.messages"))?;
    tx.commit()
        .await
        .map_err(ApiError::database("browse.commit"))?;
    let earlier_cursor = page::last(&mut rows, limit).map(|message| {
        page::encode(&Cursor::Messages {
            source_id: source,
            session_id: session,
            revision: row.revision,
            position: message.position,
        })
    });
    rows.reverse();
    let first_index = rows.first().map_or(1, |message| message.position + 1);
    Ok(Json(Transcript {
        session: row.response()?,
        messages: rows
            .into_iter()
            .map(|row| raw(row.payload))
            .collect::<Result<_, _>>()?,
        first_index,
        earlier_cursor,
    }))
}
