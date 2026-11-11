//! Artifacts: documents agents publish for people to read. See docs/artifacts.md.

use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    auth::Source,
    error::ApiError,
    page,
    reference::{self, valid_id},
    request::{self, Path, Query},
};

pub(crate) const MAX_SIZE: usize = 32 << 20;
/// The artifacts one source created may total at most this many bytes.
const QUOTA: i64 = 1 << 30;
/// Text returned to agents per read, in bytes; JSON escaping may double it.
const CHUNK: usize = 8 * 1024;
/// Bodies above this size, or of unknown size, need one of the upload permits.
const SMALL_BODY: u64 = 1 << 20;
const ID_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
const TEXT_TYPES: [&str; 4] = ["text/markdown", "text/html", "text/plain", "image/svg+xml"];
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/// The searched text; it must match the `artifacts_search` index expression.
const SEARCHED: &str = "(a.title || E'\\n' || coalesce(a.text_content, ''))";
/// Replaces the reader's own policy: artifacts may load anything, and only the
/// reader may frame them.
const CONTENT_POLICY: &str = "frame-ancestors 'self'";

#[derive(Clone)]
struct Machine {
    write: PgPool,
    read: PgPool,
    uploads: Arc<Semaphore>,
}

/// Source-token routes under `/v1/artifacts`.
pub(crate) fn machine_routes(write: PgPool, read: PgPool) -> Router {
    Router::new()
        .route("/v1/artifacts", get(find).post(create))
        .route(
            "/v1/artifacts/{id}",
            get(read_text).put(replace).patch(change),
        )
        .with_state(Machine {
            write,
            read,
            uploads: Arc::new(Semaphore::new(2)),
        })
}

/// Reader routes, behind Cloudflare Access; `read` should be the read-only pool.
pub(crate) fn reader_routes() -> Router<PgPool> {
    Router::new()
        .route("/api/artifacts", get(reader_list))
        .route("/api/artifacts/{id}", get(reader_show))
        .route("/artifacts/{id}/content", get(content))
}

fn valid_artifact_id(id: &str) -> bool {
    id.len() == 16
        && id.starts_with("art_")
        && id.bytes().skip(4).all(|byte| ID_ALPHABET.contains(&byte))
}

/// No artifact has an ID of another form.
fn artifact_id(id: String) -> Result<String, ApiError> {
    if valid_artifact_id(&id) {
        Ok(id)
    } else {
        Err(ApiError::not_found())
    }
}

fn title(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ApiError::malformed());
    }
    Ok(value.to_owned())
}

#[derive(FromRow)]
struct Row {
    id: String,
    title: String,
    media_type: String,
    revision: i64,
    size: i32,
    source_id: Uuid,
    source_alias: Option<String>,
    source_label: String,
    session_id: Option<String>,
    session_shared: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const COLUMNS: &str = concat!(
    "SELECT a.id, a.title, a.media_type, a.revision, a.size, a.source_id, \
     s.alias AS source_alias, s.label AS source_label, a.session_id, ",
    reference::shared!("a"),
    " AS session_shared, a.created_at, a.updated_at"
);
const FROM: &str = " FROM artifacts a JOIN sources s ON s.id = a.source_id";

impl Row {
    fn json(&self) -> Value {
        let source = self
            .source_alias
            .clone()
            .unwrap_or_else(|| self.source_id.to_string());
        json!({
            "id": self.id,
            "title": self.title,
            "type": self.media_type,
            "revision": self.revision.to_string(),
            "size": self.size,
            "source": source,
            "source_id": self.source_id,
            "source_label": self.source_label,
            "session": self.session_id.as_ref().map(|session| {
                reference::format(self.source_id, self.source_alias.as_deref(), session, self.session_shared)
            }),
            "session_id": self.session_id,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

async fn row(connection: &mut PgConnection, id: &str) -> Result<Row, ApiError> {
    sqlx::query_as(&format!("{COLUMNS}{FROM} WHERE a.id = $1"))
        .bind(id)
        .fetch_optional(connection)
        .await
        .map_err(ApiError::database("artifact.row"))?
        .ok_or_else(ApiError::not_found)
}

enum Content {
    Text(String),
    Binary(Vec<u8>),
}

impl Content {
    fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(bytes) => bytes.len(),
        }
    }

    /// Checks that the bytes are what the media type says.
    fn parse(media_type: &str, body: Bytes) -> Result<Self, ApiError> {
        let invalid = || ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_content");
        if TEXT_TYPES.contains(&media_type) {
            let text = String::from_utf8(body.into()).map_err(|_| invalid())?;
            return if text.contains('\0') {
                Err(invalid())
            } else {
                Ok(Self::Text(text))
            };
        }
        let signed = match media_type {
            "image/png" => body.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => body.starts_with(b"\xff\xd8\xff"),
            "image/gif" => body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a"),
            _ => body.len() >= 12 && body.starts_with(b"RIFF") && &body[8..12] == b"WEBP",
        };
        if signed {
            Ok(Self::Binary(body.into()))
        } else {
            Err(invalid())
        }
    }
}

/// The media type from `Content-Type`; text may declare only UTF-8.
fn media_type(headers: &HeaderMap) -> Result<&'static str, ApiError> {
    let unsupported = || {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_type",
        )
    };
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(unsupported)?;
    let mut parts = value.split(';');
    let essence = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let media_type = TEXT_TYPES
        .into_iter()
        .chain(IMAGE_TYPES)
        .find(|candidate| *candidate == essence)
        .ok_or_else(unsupported)?;
    for parameter in parts {
        let (name, value) = parameter.split_once('=').ok_or_else(unsupported)?;
        if !name.trim().eq_ignore_ascii_case("charset")
            || !value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
            || !TEXT_TYPES.contains(&media_type)
        {
            return Err(unsupported());
        }
    }
    Ok(media_type)
}

/// Reads a body. Large bodies take one of a few permits, so memory stays
/// bounded; small ones never wait behind a slow large upload.
async fn upload(machine: &Machine, request: Request) -> Result<Bytes, ApiError> {
    let small =
        request::content_length(request.headers()).is_some_and(|length| length <= SMALL_BODY);
    let _permit = if small {
        None
    } else {
        Some(
            machine
                .uploads
                .try_acquire()
                .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "busy"))?,
        )
    };
    request::body(request, MAX_SIZE, Duration::from_secs(120)).await
}

/// Rejects a change that would take the creating source past its quota.
async fn check_quota(
    connection: &mut PgConnection,
    source: Uuid,
    removed: i64,
    added: usize,
) -> Result<(), ApiError> {
    let used: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(size), 0)::bigint FROM artifacts WHERE source_id = $1",
    )
    .bind(source)
    .fetch_one(connection)
    .await
    .map_err(ApiError::database("artifact.quota"))?;
    if used - removed + added as i64 > QUOTA {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "artifact_quota_exceeded",
        ));
    }
    Ok(())
}

fn bind_content<'a>(
    query: sqlx::query::Query<'a, Postgres, sqlx::postgres::PgArguments>,
    content: &'a Content,
) -> sqlx::query::Query<'a, Postgres, sqlx::postgres::PgArguments> {
    match content {
        Content::Text(text) => query.bind(text.as_str()).bind(None::<&[u8]>),
        Content::Binary(bytes) => query.bind(None::<&str>).bind(bytes.as_slice()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateQuery {
    title: String,
    session: Option<String>,
}

async fn create(
    State(machine): State<Machine>,
    Extension(Source(source)): Extension<Source>,
    Query(query): Query<CreateQuery>,
    request: Request,
) -> Result<Response, ApiError> {
    let title = title(&query.title)?;
    if query
        .session
        .as_deref()
        .is_some_and(|session| !valid_id(session))
    {
        return Err(ApiError::malformed());
    }
    let media_type = media_type(request.headers())?;
    let content = Content::parse(media_type, upload(&machine, request).await?)?;
    let mut tx = machine
        .write
        .begin()
        .await
        .map_err(ApiError::database("artifact.create"))?;
    check_quota(&mut tx, source, 0, content.size()).await?;
    // 32^12 identifiers: a collision is rare, and repeated collisions are not expected.
    for _ in 0..3 {
        let id: String = {
            let mut rng = rand::rng();
            let suffix: String = (0..12)
                .map(|_| ID_ALPHABET[rng.random_range(0..ID_ALPHABET.len())] as char)
                .collect();
            format!("art_{suffix}")
        };
        let query = sqlx::query(
            "INSERT INTO artifacts (id, source_id, session_id, title, media_type, revision, size, \
             text_content, binary_content) VALUES ($1, $2, $3, $4, $5, 1, $6, $7, $8) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&id)
        .bind(source)
        .bind(query.session.as_deref())
        .bind(&title)
        .bind(media_type)
        .bind(content.size() as i32);
        let inserted = bind_content(query, &content)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database("artifact.create"))?
            .rows_affected();
        if inserted == 1 {
            let row = row(&mut tx, &id).await?;
            tx.commit()
                .await
                .map_err(ApiError::database("artifact.create"))?;
            return Ok((StatusCode::CREATED, Json(json!({"artifact": row.json()}))).into_response());
        }
    }
    Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "try_again"))
}

#[derive(FromRow)]
struct Current {
    source_id: Uuid,
    revision: i64,
    size: i32,
}

/// Locks the artifact and checks that the change is based on its current revision.
async fn current(
    connection: &mut PgConnection,
    id: &str,
    expected: i64,
) -> Result<Current, ApiError> {
    let current: Current =
        sqlx::query_as("SELECT source_id, revision, size FROM artifacts WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(ApiError::database("artifact.lock"))?
            .ok_or_else(ApiError::not_found)?;
    if current.revision != expected {
        return Err(ApiError::conflict(Some(current.revision)));
    }
    Ok(current)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceQuery {
    revision: String,
    title: Option<String>,
}

async fn replace(
    State(machine): State<Machine>,
    Path(id): Path<String>,
    Query(query): Query<ReplaceQuery>,
    request: Request,
) -> Result<Json<Value>, ApiError> {
    let id = artifact_id(id)?;
    let expected = request::revision(&query.revision)?;
    let title = query.title.as_deref().map(title).transpose()?;
    let media_type = media_type(request.headers())?;
    let content = Content::parse(media_type, upload(&machine, request).await?)?;
    let mut tx = machine
        .write
        .begin()
        .await
        .map_err(ApiError::database("artifact.replace"))?;
    let current = current(&mut tx, &id, expected).await?;
    check_quota(
        &mut tx,
        current.source_id,
        current.size.into(),
        content.size(),
    )
    .await?;
    let query = sqlx::query(
        "UPDATE artifacts SET revision = revision + 1, updated_at = now(), \
         title = coalesce($2, title), media_type = $3, size = $4, \
         text_content = $5, binary_content = $6 WHERE id = $1",
    )
    .bind(&id)
    .bind(title)
    .bind(media_type)
    .bind(content.size() as i32);
    bind_content(query, &content)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database("artifact.replace"))?;
    let row = row(&mut tx, &id).await?;
    tx.commit()
        .await
        .map_err(ApiError::database("artifact.replace"))?;
    Ok(Json(json!({"artifact": row.json()})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    old: String,
    new: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    revision: String,
    title: Option<String>,
    edits: Option<Vec<Edit>>,
}

/// Applies edits in order; each `old` must occur exactly once at that point.
fn apply(mut text: String, edits: &[Edit]) -> Result<String, ApiError> {
    for (index, edit) in edits.iter().enumerate() {
        let start = text
            .find(&edit.old)
            .ok_or_else(|| ApiError::edit("edit_not_found", index))?;
        // Overlapping occurrences count too: search again from the next character.
        let next = start + text[start..].chars().next().map_or(1, char::len_utf8);
        if text[next..].contains(&edit.old) {
            return Err(ApiError::edit("edit_not_unique", index));
        }
        text.replace_range(start..start + edit.old.len(), &edit.new);
    }
    Ok(text)
}

async fn change(
    State(machine): State<Machine>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Json<Value>, ApiError> {
    let id = artifact_id(id)?;
    request::require_json(request.headers())?;
    let change: Change = request::object(&upload(&machine, request).await?)?;
    let expected = request::revision(&change.revision)?;
    let title = change.title.as_deref().map(title).transpose()?;
    let edits = change.edits.unwrap_or_default();
    if (title.is_none() && edits.is_empty())
        || edits
            .iter()
            .any(|edit| edit.old.is_empty() || edit.new.contains('\0'))
    {
        return Err(ApiError::malformed());
    }
    let mut tx = machine
        .write
        .begin()
        .await
        .map_err(ApiError::database("artifact.change"))?;
    let current = current(&mut tx, &id, expected).await?;
    if edits.is_empty() {
        sqlx::query(
            "UPDATE artifacts SET revision = revision + 1, updated_at = now(), title = $2 \
             WHERE id = $1",
        )
        .bind(&id)
        .bind(title)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database("artifact.change"))?;
    } else {
        let text: Option<String> =
            sqlx::query_scalar("SELECT text_content FROM artifacts WHERE id = $1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await
                .map_err(ApiError::database("artifact.change"))?;
        let text =
            text.ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "not_text"))?;
        let text = apply(text, &edits)?;
        if text.len() > MAX_SIZE {
            return Err(ApiError::too_large());
        }
        check_quota(&mut tx, current.source_id, current.size.into(), text.len()).await?;
        sqlx::query(
            "UPDATE artifacts SET revision = revision + 1, updated_at = now(), \
             title = coalesce($2, title), size = $3, text_content = $4 WHERE id = $1",
        )
        .bind(&id)
        .bind(title)
        .bind(text.len() as i32)
        .bind(text)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database("artifact.change"))?;
    }
    let row = row(&mut tx, &id).await?;
    tx.commit()
        .await
        .map_err(ApiError::database("artifact.change"))?;
    Ok(Json(json!({"artifact": row.json()})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadQuery {
    offset: Option<i64>,
}

#[derive(FromRow)]
struct Chunk {
    #[sqlx(flatten)]
    row: Row,
    chunk: Option<String>,
}

async fn read_text(
    State(machine): State<Machine>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = artifact_id(id)?;
    let offset = query.offset.unwrap_or(0);
    if !(0..=MAX_SIZE as i64).contains(&offset) {
        return Err(ApiError::malformed());
    }
    // One character more than a chunk can hold shows whether more follows.
    let found: Chunk = sqlx::query_as(&format!(
        "{COLUMNS}, substr(a.text_content, $2, $3) AS chunk{FROM} WHERE a.id = $1"
    ))
    .bind(&id)
    .bind(offset as i32 + 1)
    .bind(CHUNK as i32 + 1)
    .fetch_optional(&machine.read)
    .await
    .map_err(ApiError::database("artifact.read"))?
    .ok_or_else(ApiError::not_found)?;
    let (text, next) = match found.chunk {
        None => (None, None),
        Some(chunk) => {
            let mut bytes = 0;
            let taken = chunk
                .chars()
                .take_while(|character| {
                    bytes += character.len_utf8();
                    bytes <= CHUNK
                })
                .count();
            let end = chunk
                .char_indices()
                .nth(taken)
                .map_or(chunk.len(), |(index, _)| index);
            let more = end < chunk.len();
            let mut chunk = chunk;
            chunk.truncate(end);
            (Some(chunk), more.then_some(offset + taken as i64))
        }
    };
    Ok(Json(json!({
        "artifact": found.row.json(),
        "text": text,
        "offset": offset,
        "next_offset": next,
    })))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    query: Option<String>,
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    query: Option<String>,
    limit: u16,
    updated_at: DateTime<Utc>,
    id: String,
}

/// Lists artifacts, most recently updated first. A cursor carries the query and
/// limit, so it must be used alone.
async fn list(pool: &PgPool, request: ListQuery, maximum: u16) -> Result<Value, ApiError> {
    let (query, limit, after) = match request {
        ListQuery {
            cursor: Some(cursor),
            query: None,
            limit: None,
        } => {
            let cursor: Cursor = page::decode(&cursor)?;
            if !valid_artifact_id(&cursor.id) {
                return Err(ApiError::malformed());
            }
            (
                cursor.query,
                page::limit(Some(cursor.limit), 20, maximum)?,
                Some((cursor.updated_at, cursor.id)),
            )
        }
        ListQuery {
            cursor: None,
            query,
            limit,
        } => (query, page::limit(limit, 20, maximum)?, None),
        _ => return Err(ApiError::malformed()),
    };
    let terms = query
        .as_deref()
        .map(request::terms)
        .transpose()?
        .unwrap_or_default();
    let mut sql = QueryBuilder::<Postgres>::new(COLUMNS);
    sql.push(FROM).push(" WHERE true");
    for term in &terms {
        sql.push(" AND ")
            .push(SEARCHED)
            .push(" ILIKE ")
            .push_bind(request::pattern(term));
    }
    if let Some((updated_at, id)) = &after {
        sql.push(" AND (a.updated_at, a.id) < (")
            .push_bind(updated_at)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    sql.push(" ORDER BY a.updated_at DESC, a.id DESC LIMIT ")
        .push_bind(i64::from(limit) + 1);
    let mut rows: Vec<Row> = sql
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(ApiError::database("artifact.list"))?;
    let next_cursor = page::last(&mut rows, limit).map(|last| {
        page::encode(&Cursor {
            query: query.clone(),
            limit,
            updated_at: last.updated_at,
            id: last.id.clone(),
        })
    });
    Ok(json!({
        "artifacts": rows.iter().map(Row::json).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    }))
}

async fn find(
    State(machine): State<Machine>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(list(&machine.read, query, 50).await?))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReaderListQuery {
    q: Option<String>,
    limit: Option<u16>,
    cursor: Option<String>,
}

async fn reader_list(
    State(pool): State<PgPool>,
    Query(ReaderListQuery { q, limit, cursor }): Query<ReaderListQuery>,
) -> Result<Json<Value>, ApiError> {
    let query = q.filter(|q| !q.trim().is_empty());
    Ok(Json(
        list(
            &pool,
            ListQuery {
                query,
                limit,
                cursor,
            },
            100,
        )
        .await?,
    ))
}

async fn reader_show(
    State(pool): State<PgPool>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = artifact_id(id)?;
    let mut connection = pool
        .acquire()
        .await
        .map_err(ApiError::database("artifact.show"))?;
    Ok(Json(
        json!({"artifact": row(&mut connection, &id).await?.json()}),
    ))
}

#[derive(FromRow)]
struct Body {
    media_type: String,
    text_content: Option<String>,
    binary_content: Option<Vec<u8>>,
}

/// The raw content, isolated in an opaque origin.
async fn content(State(pool): State<PgPool>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = artifact_id(id)?;
    let body: Body = sqlx::query_as(
        "SELECT media_type, text_content, binary_content FROM artifacts WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&pool)
    .await
    .map_err(ApiError::database("artifact.content"))?
    .ok_or_else(ApiError::not_found)?;
    let content_type = match body.media_type.as_str() {
        "text/html" => "text/html; charset=utf-8",
        "image/svg+xml" => "image/svg+xml; charset=utf-8",
        // Markdown is rendered by the reader; raw, it is shown as text.
        "text/markdown" | "text/plain" => "text/plain; charset=utf-8",
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        _ => "image/webp",
    };
    let bytes: Vec<u8> = match (body.text_content, body.binary_content) {
        (Some(text), _) => text.into_bytes(),
        (None, Some(bytes)) => bytes,
        (None, None) => return Err(ApiError::not_found()),
    };
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CONTENT_POLICY),
            ),
            (
                header::X_FRAME_OPTIONS,
                HeaderValue::from_static("SAMEORIGIN"),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Edit {
        Edit {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn edits_apply_in_order_and_require_one_match() {
        let text = "alpha beta gamma".to_owned();
        assert_eq!(
            apply(
                text.clone(),
                &[edit("beta", "delta"), edit("delta gamma", "ok")]
            )
            .ok()
            .unwrap(),
            "alpha ok"
        );
        let error =
            |result: Result<String, ApiError>| result.err().unwrap().into_response().status();
        assert_eq!(
            error(apply("a a".into(), &[edit("a", "b")])),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            error(apply("aaa".into(), &[edit("aa", "b")])),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            apply("é aé".into(), &[edit("é a", "x")]).ok().unwrap(),
            "xé"
        );
        assert_eq!(
            error(apply(text, &[edit("beta", "x"), edit("beta", "y")])),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn content_matches_its_type() {
        let parse = |media_type: &str, bytes: &[u8]| {
            Content::parse(media_type, Bytes::copy_from_slice(bytes)).is_ok()
        };
        assert!(parse("text/markdown", "# Plan ✓".as_bytes()));
        assert!(!parse("text/plain", b"\xff"));
        assert!(!parse("text/plain", b"nul\0"));
        assert!(parse("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(!parse("image/png", b"GIF89a"));
        assert!(parse("image/webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!parse("image/webp", b"RIFF"));
        assert!(parse("image/svg+xml", b"<svg/>"));
    }

    #[test]
    fn media_types_allow_only_utf8_text() {
        let parse = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(value).unwrap());
            media_type(&headers).ok()
        };
        assert_eq!(parse("text/HTML; charset=UTF-8"), Some("text/html"));
        assert_eq!(parse("image/png"), Some("image/png"));
        assert_eq!(parse("text/plain; charset=latin1"), None);
        assert_eq!(parse("image/png; charset=utf-8"), None);
        assert_eq!(parse("application/json"), None);
    }
}
