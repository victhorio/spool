use std::{collections::BTreeMap, net::IpAddr, path::Path as FilePath};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    extract::{
        Path, Query, Request, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sqlx::{FromRow, PgPool};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

use crate::{error::ApiError, snapshot::valid_id};

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback() || (ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

pub fn validate_browse_bind(bind: IpAddr) -> anyhow::Result<()> {
    if !private_ip(bind) {
        bail!(
            "browse is unauthenticated: --bind must be loopback or a Tailscale IP (100.64.0.0/10 or fd7a:115c:a1e0::/48); wildcard and LAN/public binds are not allowed"
        );
    }
    Ok(())
}

/// An unauthenticated, read-only listener; never mount this on the ingestion router.
/// Use `connect_browse` and bind only to loopback or a private tailnet address.
pub fn browse_router(pool: PgPool, assets: impl AsRef<FilePath>) -> anyhow::Result<Router> {
    let assets = assets.as_ref();
    let index = assets.join("index.html");
    let metadata = std::fs::metadata(&index).with_context(|| {
        format!(
            "browse assets missing: {} is required; build the web frontend first or set --assets",
            index.display()
        )
    })?;
    if !metadata.is_file() {
        bail!("browse assets invalid: {} must be a file", index.display());
    }
    Ok(Router::new()
        .route("/api/sessions", get(sessions))
        .route("/api/sources/{source}/sessions/{session}", get(session))
        .route_service("/", ServeFile::new(index.clone()))
        .route_service("/index.html", ServeFile::new(index))
        .nest_service("/assets", ServeDir::new(assets.join("assets")))
        .route_service("/favicon.ico", ServeFile::new(assets.join("favicon.ico")))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "not_found") })
        .method_not_allowed_fallback(|| async {
            ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        })
        .with_state(pool)
        .layer(middleware::from_fn(privacy)))
}

async fn privacy(request: Request, next: Next) -> Response {
    let mut response = if trusted_request(&request) {
        next.run(request).await
    } else {
        ApiError::new(StatusCode::FORBIDDEN, "forbidden_origin").into_response()
    };
    let headers = response.headers_mut();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("cross-origin-resource-policy", "same-origin"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; frame-src 'none'",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn trusted_request(request: &Request) -> bool {
    let headers = request.headers();
    let mut hosts = headers.get_all("host").iter();
    let Some(host) = hosts.next().and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if hosts.next().is_some() {
        return false;
    }
    let Ok(authority) = host.parse::<Authority>() else {
        return false;
    };
    if host.contains('@') || (host.len() > authority.host().len() && authority.port_u16().is_none())
    {
        return false;
    }
    let name = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    // Literal private IPs (and localhost) prevent a rebinding domain from becoming
    // a trusted same-origin reader. Forwarded headers are deliberately ignored.
    if !name.eq_ignore_ascii_case("localhost") && !name.parse::<IpAddr>().is_ok_and(private_ip) {
        return false;
    }
    if request.uri().path() == "/api" || request.uri().path().starts_with("/api/") {
        if headers.get_all("sec-fetch-site").iter().any(|value| {
            !value
                .to_str()
                .is_ok_and(|value| matches!(value, "same-origin" | "none"))
        }) {
            return false;
        }
        let expected = format!("http://{authority}");
        if headers
            .get_all("origin")
            .iter()
            .any(|value| value.as_bytes() != expected.as_bytes())
        {
            return false;
        }
    }
    true
}

#[derive(Deserialize)]
struct Page {
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Cursor {
    SessionsV2 {
        activity: i64,
        source_id: Uuid,
        session_id: String,
    },
    MessagesV1 {
        source_id: Uuid,
        session_id: String,
        revision: i64,
        position: i32,
    },
}

impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("cursor is serializable"))
    }

    fn decode(value: &str) -> Result<Self, ApiError> {
        if value.len() > 8192 {
            return Err(ApiError::malformed());
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| ApiError::malformed())?;
        serde_json::from_slice(&bytes).map_err(|_| ApiError::malformed())
    }
}

#[derive(FromRow)]
struct SessionRow {
    source_id: Uuid,
    source_label: String,
    session_id: String,
    revision: i64,
    info: String,
    received_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct Session {
    source_id: Uuid,
    source_label: String,
    session_id: String,
    revision: String,
    info: Box<RawValue>,
    received_at: DateTime<Utc>,
}

fn raw(value: String) -> Result<Box<RawValue>, ApiError> {
    RawValue::from_string(value)
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "invalid_archive"))
}

impl SessionRow {
    fn response(self) -> Result<Session, ApiError> {
        Ok(Session {
            source_id: self.source_id,
            source_label: self.source_label,
            session_id: self.session_id,
            revision: self.revision.to_string(),
            info: raw(self.info)?,
            received_at: self.received_at,
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

fn page_limit(page: Page, default: u16, max: u16) -> Result<(Page, u16), ApiError> {
    let limit = page.limit.unwrap_or(default);
    if !(1..=max).contains(&limit) {
        return Err(ApiError::malformed());
    }
    Ok((page, limit))
}

async fn sessions(
    State(pool): State<PgPool>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Sessions>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::malformed())?;
    let (page, limit) = page_limit(
        Page {
            cursor: query.cursor,
            limit: query.limit,
        },
        50,
        500,
    )?;
    let after = page.cursor.as_deref().map(Cursor::decode).transpose()?;
    let after = match after {
        Some(Cursor::SessionsV2 {
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
    let mut stream = sqlx::query_as::<_, SessionRow>(
        "SELECT s.source_id, sources.label AS source_label, s.session_id, s.revision, \
         s.info::text AS info, s.received_at FROM sessions s \
         JOIN sources ON sources.id = s.source_id \
         WHERE s.deleted_at IS NULL AND ($1::uuid IS NULL OR s.source_id = $1)",
    )
    .bind(query.source)
    .fetch(&pool);
    let mut recent = BTreeMap::new();
    while let Some(row) = stream
        .try_next()
        .await
        .map_err(|error| ApiError::database("browse.sessions", error))?
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
    let next_cursor = if rows.len() > usize::from(limit) {
        rows.pop();
        rows.last().map(|((activity, source, session), _)| {
            Cursor::SessionsV2 {
                activity: *activity,
                source_id: *source,
                session_id: session.clone(),
            }
            .encode()
        })
    } else {
        None
    };
    let sources = sqlx::query_as(
        "SELECT id AS source_id, label AS source_label FROM sources \
         WHERE EXISTS (SELECT 1 FROM sessions WHERE source_id = sources.id AND deleted_at IS NULL) \
         ORDER BY label, id",
    )
    .fetch_all(&pool)
    .await
    .map_err(|error| ApiError::database("browse.sources", error))?;
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
    next_cursor: Option<String>,
}

#[derive(FromRow)]
struct MessageRow {
    position: i32,
    payload: String,
}

async fn session(
    State(pool): State<PgPool>,
    path: Result<Path<(Uuid, String)>, PathRejection>,
    query: Result<Query<Page>, QueryRejection>,
) -> Result<Json<Transcript>, ApiError> {
    let Path((source, session)) = path.map_err(|_| ApiError::malformed())?;
    if !valid_id(&session) {
        return Err(ApiError::malformed());
    }
    let Query(page) = query.map_err(|_| ApiError::malformed())?;
    let (page, limit) = page_limit(page, 100, 200)?;
    let cursor = page.cursor.as_deref().map(Cursor::decode).transpose()?;
    let (expected_revision, position) = match cursor {
        Some(Cursor::MessagesV1 {
            source_id,
            session_id,
            revision,
            position,
        }) if source_id == source && session_id == session && revision > 0 && position >= 0 => {
            (Some(revision), position)
        }
        None => (None, -1),
        _ => return Err(ApiError::malformed()),
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| ApiError::database("browse.begin", error))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|error| ApiError::database("browse.snapshot", error))?;
    let row: SessionRow = sqlx::query_as(
        "SELECT s.source_id, sources.label AS source_label, s.session_id, s.revision, \
         s.info::text AS info, s.received_at FROM sessions s \
         JOIN sources ON sources.id = s.source_id \
         WHERE s.source_id = $1 AND s.session_id = $2 AND s.deleted_at IS NULL",
    )
    .bind(source)
    .bind(&session)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| ApiError::database("browse.session", error))?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))?;
    if expected_revision.is_some_and(|revision| revision != row.revision) {
        return Err(ApiError::conflict(Some(row.revision)));
    }
    let mut rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT position, payload::text AS payload FROM messages \
         WHERE source_id = $1 AND session_id = $2 AND position > $3 ORDER BY position LIMIT $4",
    )
    .bind(source)
    .bind(&session)
    .bind(position)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await
    .map_err(|error| ApiError::database("browse.messages", error))?;
    tx.commit()
        .await
        .map_err(|error| ApiError::database("browse.commit", error))?;
    let next_cursor = if rows.len() > usize::from(limit) {
        rows.pop();
        rows.last().map(|message| {
            Cursor::MessagesV1 {
                source_id: source,
                session_id: session,
                revision: row.revision,
                position: message.position,
            }
            .encode()
        })
    } else {
        None
    };
    Ok(Json(Transcript {
        session: row.response()?,
        messages: rows
            .into_iter()
            .map(|row| raw(row.payload))
            .collect::<Result<_, _>>()?,
        next_cursor,
    }))
}
