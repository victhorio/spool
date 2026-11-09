use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    routing::post,
};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{error::ApiError, snapshot::valid_id};

mod cursor;
use cursor::{Cursor, FindKey};

pub(super) const PAGE_BYTES: usize = 24 * 1024;
const MAX_POSITION: i32 = 100_000;

pub(crate) fn routes() -> Router<PgPool> {
    Router::new()
        .route("/api/agent/find", post(find))
        .route("/api/agent/search", post(search))
        .route("/api/agent/read", post(read))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Request<T> {
    Initial(T),
    Continuation(Continuation),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Continuation {
    cursor: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Find {
    query: Option<String>,
    repository: Option<String>,
    #[serde(default)]
    include_tools: bool,
    limit: Option<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    session: String,
    query: String,
    revision: Option<String>,
    #[serde(default)]
    include_tools: bool,
    limit: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    session: String,
    revision: Option<String>,
    start: Option<i32>,
    end: Option<i32>,
}

fn terms(query: Option<&str>) -> Result<Vec<String>, ApiError> {
    let Some(query) = query else {
        return Ok(Vec::new());
    };
    if query.len() > 512 || query.contains('\0') {
        return Err(ApiError::malformed());
    }
    let terms: Vec<_> = query.split_whitespace().map(str::to_owned).collect();
    if terms.is_empty() || terms.len() > 16 {
        return Err(ApiError::malformed());
    }
    Ok(terms)
}

fn limit(value: Option<u16>, default: u16, maximum: u16) -> Result<usize, ApiError> {
    let value = value.unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(ApiError::malformed());
    }
    Ok(usize::from(value))
}

fn revision(value: Option<&str>) -> Result<Option<i64>, ApiError> {
    value
        .map(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(ApiError::malformed)
        })
        .transpose()
}

fn pattern(term: &str) -> String {
    format!(
        "%{}%",
        term.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn matches(sql: &mut QueryBuilder<'_, Postgres>, column: &str, terms: &[String]) {
    sql.push("(");
    for (index, term) in terms.iter().enumerate() {
        if index > 0 {
            sql.push(" AND ");
        }
        sql.push(column).push(" ILIKE ").push_bind(pattern(term));
    }
    sql.push(")");
}

fn metadata_matches(sql: &mut QueryBuilder<'_, Postgres>, terms: &[String]) {
    sql.push("(");
    matches(sql, "s.retrieval_title", terms);
    sql.push(" OR ");
    matches(sql, "s.session_id", terms);
    sql.push(")");
}

fn search_column(include_tools: bool) -> &'static str {
    if include_tools {
        "(m.conversation_text || E'\\n' || m.tool_text)"
    } else {
        "m.conversation_text"
    }
}

// Searchable spans map excerpts back to evidence without selecting excluded reasoning or metadata.
fn passage(sql: &mut QueryBuilder<'_, Postgres>, first_term: &str, include_tools: bool) {
    sql.push(" CROSS JOIN LATERAL (SELECT span.start + octet_length(left(span.text, hit.start))::bigint AS hit_offset, \
        substring(span.text FROM hit.start + 1 FOR 300) AS hit_excerpt FROM \
        jsonb_array_elements(m.conversation_spans");
    if include_tools {
        sql.push(" || m.tool_spans");
    }
    sql.push(") WITH ORDINALITY ranges(value, ordinal) CROSS JOIN LATERAL \
        (SELECT (ranges.value->>0)::integer AS start, \
        convert_from(substring(convert_to(m.retrieval_text, 'UTF8') FROM \
        (ranges.value->>0)::integer + 1 FOR (ranges.value->>1)::integer - (ranges.value->>0)::integer), 'UTF8') AS text) span \
        CROSS JOIN LATERAL (SELECT strpos(lower(span.text), lower(")
        .push_bind(first_term.to_owned())
        .push(")) AS position) found CROSS JOIN LATERAL \
        (SELECT greatest(found.position - 129, 0) AS start) hit \
        WHERE found.position > 0 ORDER BY ranges.ordinal LIMIT 1) evidence ");
}

#[derive(FromRow, Serialize)]
struct Coverage {
    total_sessions: i64,
    indexed_sessions: i64,
    unassigned_repository_sessions: i64,
}

async fn coverage(pool: &PgPool) -> Result<Coverage, ApiError> {
    sqlx::query_as(
        "SELECT count(*) AS total_sessions, \
         count(retrieval_title) AS indexed_sessions, \
         count(*) FILTER (WHERE repository IS NULL) AS unassigned_repository_sessions \
         FROM sessions WHERE deleted_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| ApiError::database("retrieval.coverage", error))
}

#[derive(FromRow)]
struct FindRow {
    source_id: Uuid,
    alias: Option<String>,
    session_id: String,
    revision: i64,
    title: String,
    repository: Option<String>,
    captured_at: DateTime<Utc>,
    activity: i64,
    message_count: i64,
    score: i32,
    hit_position: Option<i32>,
    hit_offset: Option<i64>,
    hit_excerpt: Option<String>,
}

fn reference(source: Uuid, alias: Option<&str>, session: &str) -> String {
    match alias {
        Some(alias) => format!("{alias}/{session}"),
        None => format!("{source}/{session}"),
    }
}

fn read_cursor(source: Uuid, session: String, revision: i64, index: i32, offset: i64) -> String {
    Cursor::Read {
        source,
        session,
        revision,
        start: index,
        end: index,
        offset,
    }
    .encode()
}

fn fits(value: &Value) -> bool {
    serde_json::to_vec(value).expect("serializable page").len() <= PAGE_BYTES
}

async fn find(
    State(pool): State<PgPool>,
    request: Result<Json<Request<Find>>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::malformed())?;
    let (query, after) = match request {
        Request::Initial(query) => (query, None),
        Request::Continuation(value) => match Cursor::decode(&value.cursor)? {
            Cursor::Find { query, after }
                if valid_id(&after.session) && (0..=2).contains(&after.score) =>
            {
                (query, Some(after))
            }
            _ => return Err(ApiError::malformed()),
        },
    };
    let terms = terms(query.query.as_deref())?;
    let limit = limit(query.limit, 10, 50)?;
    if query
        .repository
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 1024 || value.contains('\0'))
    {
        return Err(ApiError::malformed());
    }
    let mut sql = QueryBuilder::new(
        "WITH ranked AS (SELECT s.source_id, sources.alias, s.session_id, s.revision, \
         s.retrieval_title AS title, s.repository, s.captured_at, s.activity, \
         (SELECT count(*) FROM messages m WHERE m.source_id = s.source_id \
          AND m.session_id = s.session_id) AS message_count, ",
    );
    if terms.is_empty() {
        sql.push("0");
    } else {
        sql.push("CASE WHEN ");
        metadata_matches(&mut sql, &terms);
        sql.push(" THEN 2 ELSE 1 END");
    }
    sql.push(
        "::integer AS score FROM sessions s JOIN sources ON sources.id = s.source_id \
        WHERE s.deleted_at IS NULL AND s.retrieval_title IS NOT NULL",
    );
    if let Some(repository) = &query.repository {
        sql.push(" AND s.repository = ")
            .push_bind(repository.clone());
    }
    if !terms.is_empty() {
        sql.push(" AND (");
        metadata_matches(&mut sql, &terms);
        sql.push(" OR EXISTS (SELECT 1 FROM messages m WHERE m.source_id = s.source_id AND m.session_id = s.session_id AND ");
        matches(&mut sql, search_column(query.include_tools), &terms);
        sql.push("))");
    }
    sql.push(") SELECT s.*, ");
    if terms.is_empty() {
        sql.push("NULL::integer AS hit_position, NULL::bigint AS hit_offset, NULL::text AS hit_excerpt FROM ranked s");
    } else {
        sql.push("hit.position AS hit_position, hit.hit_offset, hit.hit_excerpt FROM ranked s LEFT JOIN LATERAL \
            (SELECT m.position, evidence.hit_offset, evidence.hit_excerpt FROM messages m");
        passage(&mut sql, &terms[0], query.include_tools);
        sql.push(" WHERE m.source_id = s.source_id AND m.session_id = s.session_id AND ");
        matches(&mut sql, search_column(query.include_tools), &terms);
        sql.push(" ORDER BY m.position DESC LIMIT 1) hit ON true");
    }
    if let Some(after) = after {
        sql.push(" WHERE (s.score, s.activity, s.source_id, s.session_id) < (")
            .push_bind(after.score)
            .push(", ")
            .push_bind(after.activity)
            .push(", ")
            .push_bind(after.source)
            .push(", ")
            .push_bind(after.session)
            .push(")");
    }
    sql.push(" ORDER BY s.score DESC, s.activity DESC, s.source_id DESC, s.session_id DESC LIMIT ")
        .push_bind(limit as i64 + 1);
    let coverage = coverage(&pool).await?;
    let mut response = json!({"sessions": [], "coverage": coverage, "next_cursor": null});
    let mut stream = sql.build_query_as::<FindRow>().fetch(&pool);
    let mut last_cursor = None;
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(|error| ApiError::database("retrieval.find", error))?
    {
        if response["sessions"].as_array().unwrap().len() == limit {
            response["next_cursor"] = json!(last_cursor);
            break;
        }
        let session = reference(row.source_id, row.alias.as_deref(), &row.session_id);
        let matched = match (row.hit_position, row.hit_offset, row.hit_excerpt) {
            (Some(position), Some(offset), Some(excerpt)) => json!({
                "index": position + 1, "excerpt": excerpt,
                "read_cursor": read_cursor(row.source_id, row.session_id.clone(), row.revision, position + 1, offset),
            }),
            _ => Value::Null,
        };
        let cursor = Cursor::Find {
            query: query.clone(),
            after: FindKey {
                score: row.score,
                activity: row.activity,
                source: row.source_id,
                session: row.session_id,
            },
        }
        .encode();
        response["sessions"].as_array_mut().unwrap().push(json!({
            "session": session, "title": row.title.chars().take(512).collect::<String>(),
            "repository": row.repository, "revision": row.revision.to_string(),
            "captured_at": row.captured_at, "message_count": row.message_count, "match": matched,
        }));
        // Reserve the continuation before accepting an entry: it can be several KiB.
        response["next_cursor"] = json!(cursor);
        if !fits(&response) {
            response["sessions"].as_array_mut().unwrap().pop();
            response["next_cursor"] = json!(last_cursor);
            break;
        }
        last_cursor = Some(cursor);
        response["next_cursor"] = Value::Null;
    }
    Ok(Json(response))
}

#[derive(FromRow)]
struct Session {
    source_id: Uuid,
    session_id: String,
    alias: Option<String>,
    revision: i64,
    captured_at: DateTime<Utc>,
    retrieval_title: Option<String>,
    message_count: i64,
}

async fn session(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    input: &str,
    expected: Option<i64>,
) -> Result<Session, ApiError> {
    let (qualifier, id) = match input.split_once('/') {
        Some((source, id)) => (Some(source), id),
        None => (None, input),
    };
    if !valid_id(id) {
        return Err(ApiError::malformed());
    }
    let mut sql = QueryBuilder::new(
        "SELECT s.source_id, s.session_id, sources.alias, s.revision, s.captured_at, s.retrieval_title, \
         (SELECT count(*) FROM messages m WHERE m.source_id = s.source_id AND m.session_id = s.session_id) AS message_count \
         FROM sessions s JOIN sources ON sources.id = s.source_id WHERE s.deleted_at IS NULL AND s.session_id = ",
    );
    sql.push_bind(id);
    if let Some(qualifier) = qualifier {
        if qualifier.len() == 36 {
            let source = qualifier
                .parse::<Uuid>()
                .map_err(|_| ApiError::malformed())?;
            sql.push(" AND s.source_id = ").push_bind(source);
        } else if qualifier.len() <= 32
            && qualifier.starts_with(|c: char| c.is_ascii_lowercase())
            && qualifier
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            sql.push(" AND sources.alias = ").push_bind(qualifier);
        } else {
            return Err(ApiError::malformed());
        }
    }
    sql.push(" ORDER BY s.source_id LIMIT 11");
    let mut sessions: Vec<Session> = sql
        .build_query_as()
        .fetch_all(&mut **tx)
        .await
        .map_err(|error| ApiError::database("retrieval.session", error))?;
    if sessions.len() > 1 {
        return Err(ApiError::ambiguous_session(
            sessions
                .iter()
                .take(10)
                .map(|s| reference(s.source_id, s.alias.as_deref(), &s.session_id))
                .collect(),
            sessions.len() > 10,
        ));
    }
    let session = sessions
        .pop()
        .ok_or_else(|| ApiError::new(axum::http::StatusCode::NOT_FOUND, "not_found"))?;
    if expected.is_some_and(|revision| revision != session.revision) {
        return Err(ApiError::session_changed(session.revision));
    }
    if session.retrieval_title.is_none() {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "not_indexed",
        ));
    }
    Ok(session)
}

async fn transaction(pool: &PgPool) -> Result<sqlx::Transaction<'_, Postgres>, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| ApiError::database("retrieval.begin", error))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|error| ApiError::database("retrieval.snapshot", error))?;
    Ok(tx)
}

#[derive(FromRow)]
struct Match {
    position: i32,
    hit_offset: i64,
    hit_excerpt: String,
}

async fn search(
    State(pool): State<PgPool>,
    request: Result<Json<Request<Search>>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::malformed())?;
    let (query, expected, before) = match request {
        Request::Initial(query) => {
            let revision = revision(query.revision.as_deref())?;
            (query, revision, MAX_POSITION)
        }
        Request::Continuation(value) => match Cursor::decode(&value.cursor)? {
            Cursor::Search {
                source,
                session,
                query,
                revision,
                include_tools,
                limit,
                before,
            } if revision > 0 && (0..MAX_POSITION).contains(&before) => (
                Search {
                    session: format!("{source}/{session}"),
                    query,
                    revision: None,
                    include_tools,
                    limit,
                },
                Some(revision),
                before,
            ),
            _ => return Err(ApiError::malformed()),
        },
    };
    let terms = terms(Some(&query.query))?;
    let limit = limit(query.limit, 20, 100)?;
    let mut tx = transaction(&pool).await?;
    let state = session(&mut tx, &query.session, expected).await?;
    let reference = reference(state.source_id, state.alias.as_deref(), &state.session_id);
    let mut sql = QueryBuilder::new(
        "SELECT m.position, evidence.hit_offset, evidence.hit_excerpt FROM messages m",
    );
    passage(&mut sql, &terms[0], query.include_tools);
    sql.push(" WHERE m.source_id = ")
        .push_bind(state.source_id)
        .push(" AND m.session_id = ")
        .push_bind(state.session_id.clone())
        .push(" AND m.position < ")
        .push_bind(before)
        .push(" AND ");
    matches(&mut sql, search_column(query.include_tools), &terms);
    sql.push(" ORDER BY m.position DESC LIMIT ")
        .push_bind(limit as i64 + 1);
    let mut response = json!({
        "session": reference, "revision": state.revision.to_string(),
        "captured_at": state.captured_at, "message_count": state.message_count,
        "matches": [], "next_cursor": null,
    });
    let mut stream = sql.build_query_as::<Match>().fetch(&mut *tx);
    let mut last_cursor = None;
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(|error| ApiError::database("retrieval.search", error))?
    {
        if response["matches"].as_array().unwrap().len() == limit {
            response["next_cursor"] = json!(last_cursor);
            break;
        }
        let cursor = Cursor::Search {
            source: state.source_id,
            session: state.session_id.clone(),
            query: query.query.clone(),
            revision: state.revision,
            include_tools: query.include_tools,
            limit: query.limit,
            before: row.position,
        }
        .encode();
        response["matches"].as_array_mut().unwrap().push(json!({
            "index": row.position + 1, "excerpt": row.hit_excerpt,
            "read_cursor": read_cursor(state.source_id, state.session_id.clone(), state.revision, row.position + 1, row.hit_offset),
        }));
        response["next_cursor"] = json!(cursor);
        if !fits(&response) {
            response["matches"].as_array_mut().unwrap().pop();
            response["next_cursor"] = json!(last_cursor);
            break;
        }
        last_cursor = Some(cursor);
        response["next_cursor"] = Value::Null;
    }
    drop(stream);
    tx.commit()
        .await
        .map_err(|error| ApiError::database("retrieval.commit", error))?;
    Ok(Json(response))
}

#[derive(FromRow)]
struct Chunk {
    position: i32,
    message_id: String,
    role: Option<String>,
    total_bytes: Option<i64>,
    chunk: Option<Vec<u8>>,
}

async fn read(
    State(pool): State<PgPool>,
    request: Result<Json<Request<Read>>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(request) = request.map_err(|_| ApiError::malformed())?;
    let (input, expected, start, end, offset) = match request {
        Request::Initial(query) => {
            let start = query.start.unwrap_or(1);
            (
                query.session,
                revision(query.revision.as_deref())?,
                start,
                query.end.unwrap_or(start),
                0,
            )
        }
        Request::Continuation(value) => match Cursor::decode(&value.cursor)? {
            Cursor::Read {
                source,
                session,
                revision,
                start,
                end,
                offset,
            } if revision > 0 => (
                format!("{source}/{session}"),
                Some(revision),
                start,
                end,
                offset,
            ),
            _ => return Err(ApiError::malformed()),
        },
    };
    if start < 1 || end < start || end > MAX_POSITION || !(0..=64 * 1024 * 1024).contains(&offset) {
        return Err(ApiError::malformed());
    }
    let mut tx = transaction(&pool).await?;
    let state = session(&mut tx, &input, expected).await?;
    let reference = reference(state.source_id, state.alias.as_deref(), &state.session_id);
    let end = end.min(state.message_count as i32);
    let mut response = json!({
        "session": reference, "revision": state.revision.to_string(),
        "captured_at": state.captured_at, "message_count": state.message_count,
        "messages": [], "next_cursor": null,
    });
    let mut stream = sqlx::query_as::<_, Chunk>(
        "SELECT position, message_id, retrieval_role AS role, \
         octet_length(retrieval_text)::bigint AS total_bytes, \
         substring(convert_to(retrieval_text, 'UTF8') FROM \
           (CASE WHEN position = $3 THEN $5 ELSE 0 END)::integer + 1 FOR $6) AS chunk \
         FROM messages WHERE source_id = $1 AND session_id = $2 \
         AND position >= $3 AND position < $4 ORDER BY position LIMIT 200",
    )
    .bind(state.source_id)
    .bind(state.session_id.clone())
    .bind(start - 1)
    .bind(end)
    .bind(offset)
    .bind(PAGE_BYTES as i32)
    .fetch(&mut *tx);
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(|error| ApiError::database("retrieval.read", error))?
    {
        let (Some(bytes), Some(total), Some(role)) = (row.chunk, row.total_bytes, row.role) else {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "not_indexed",
            ));
        };
        let offset = if row.position == start - 1 { offset } else { 0 };
        if offset > total {
            return Err(ApiError::malformed());
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap()
            }
            Err(_) => return Err(ApiError::malformed()),
        };
        let mut boundaries: Vec<_> = text.char_indices().map(|(index, _)| index).collect();
        boundaries.push(text.len());
        let mut low = 0;
        let mut high = boundaries.len() - 1;
        let entry = json!({
            "index": row.position + 1, "id": row.message_id, "role": role,
            "text": "", "offset_bytes": offset, "total_bytes": total,
        });
        response["messages"].as_array_mut().unwrap().push(entry);
        let last = response["messages"].as_array().unwrap().len() - 1;
        let continuation = |length: usize| {
            let next_offset = offset + length as i64;
            if next_offset < total {
                Some(Cursor::Read {
                    source: state.source_id,
                    session: state.session_id.clone(),
                    revision: state.revision,
                    start: row.position + 1,
                    end,
                    offset: next_offset,
                })
            } else if row.position + 1 < end {
                Some(Cursor::Read {
                    source: state.source_id,
                    session: state.session_id.clone(),
                    revision: state.revision,
                    start: row.position + 2,
                    end,
                    offset: 0,
                })
            } else {
                None
            }
            .map(|cursor| cursor.encode())
        };
        while low < high {
            let middle = (low + high).div_ceil(2);
            response["messages"][last]["text"] = json!(&text[..boundaries[middle]]);
            response["next_cursor"] = json!(continuation(boundaries[middle]));
            if fits(&response) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let length = boundaries[low];
        response["messages"][last]["text"] = json!(&text[..length]);
        response["next_cursor"] = json!(continuation(length));
        if (low == 0 && !text.is_empty()) || !fits(&response) {
            response["messages"].as_array_mut().unwrap().pop();
            response["next_cursor"] = json!(
                Cursor::Read {
                    source: state.source_id,
                    session: state.session_id.clone(),
                    revision: state.revision,
                    start: row.position + 1,
                    end,
                    offset,
                }
                .encode()
            );
            break;
        }
        if offset + (length as i64) < total {
            break;
        }
    }
    drop(stream);
    tx.commit()
        .await
        .map_err(|error| ApiError::database("retrieval.commit", error))?;
    if !fits(&response) {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "response_too_large",
        ));
    }
    Ok(Json(response))
}
