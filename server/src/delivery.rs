//! Messages for sessions on machines that are online. Each machine long-polls
//! for its sessions' messages; a poll is also its heartbeat, reports the
//! sessions running there and acknowledges the messages it delivered.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, QueryBuilder};
use tokio::{sync::Notify, time::Instant};
use uuid::Uuid;

use crate::{
    auth,
    error::ApiError,
    reference::{self, Reference},
    snapshot::valid_id,
};

/// SQL for the status of session `s` on its source `sources`: `offline` unless
/// the machine polled within the last minute, then `running` or `idle`.
macro_rules! status {
    () => {
        "CASE WHEN sources.polled_at > now() - interval '60 seconds' THEN \
         CASE WHEN s.session_id = ANY(sources.running) THEN 'running' ELSE 'idle' END \
         ELSE 'offline' END"
    };
}
pub(crate) use status;

/// How long a poll waits for a message. Machines poll again right away, so
/// this stays well inside the one-minute window that keeps them online.
const WAIT: Duration = Duration::from_secs(25);
const BATCH: i64 = 20;
const MAX_TEXT: usize = 64 * 1024;
/// Room for the text after JSON escaping, which can be six bytes per byte.
const MAX_BODY: usize = 6 * MAX_TEXT + 1024;

/// Undelivered messages and the signal that wakes waiting polls. Polls wait in
/// this process, so the service runs as a single instance.
#[derive(Clone)]
pub(crate) struct Mailbox {
    pool: PgPool,
    sent: Arc<Notify>,
}

impl Mailbox {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            sent: Arc::new(Notify::new()),
        }
    }
}

pub(crate) fn machine_routes(mailbox: Mailbox) -> Router {
    Router::new()
        .route("/v1/poll", post(poll))
        .route("/v1/messages", post(send_from_agent))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(mailbox)
}

/// Mounted only in the reader, behind Cloudflare Access.
pub(crate) fn reader_routes() -> Router<Mailbox> {
    Router::new()
        .route("/api/messages", post(send_from_owner))
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

fn database(operation: &'static str) -> impl FnOnce(sqlx::Error) -> ApiError {
    move |error| ApiError::database(operation, error)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Poll {
    running: Vec<String>,
    delivered: Vec<Uuid>,
}

#[derive(FromRow)]
struct Pending {
    id: Uuid,
    session_id: String,
    text: String,
    sender_session_id: Option<String>,
    sender_name: Option<String>,
}

/// Messages a machine has not collected within ten minutes are dropped, so a
/// machine that comes back later does not act on stale instructions.
async fn poll(
    State(mailbox): State<Mailbox>,
    headers: HeaderMap,
    body: Result<Json<Poll>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let source = auth::authenticate(&mailbox.pool, &headers).await?;
    let Json(poll) = body.map_err(|_| ApiError::malformed())?;
    if poll.running.len() > 100
        || poll.delivered.len() > 100
        || !poll.running.iter().all(|id| valid_id(id))
    {
        return Err(ApiError::malformed());
    }
    sqlx::query(
        "WITH done AS (DELETE FROM deliveries WHERE source_id = $1 \
         AND (id = ANY($2) OR created_at < now() - interval '10 minutes')) \
         UPDATE sources SET polled_at = now(), running = $3 WHERE id = $1",
    )
    .bind(source)
    .bind(&poll.delivered)
    .bind(&poll.running)
    .execute(&mailbox.pool)
    .await
    .map_err(database("delivery.poll"))?;
    let deadline = Instant::now() + WAIT;
    loop {
        // Listen before reading, so a message sent in between still wakes this poll.
        let sent = mailbox.sent.notified();
        tokio::pin!(sent);
        sent.as_mut().enable();
        let pending: Vec<Pending> = sqlx::query_as(
            "SELECT d.id, d.session_id, d.text, d.sender_session_id, \
             coalesce(sender.alias, sender.label) AS sender_name \
             FROM deliveries d LEFT JOIN sources sender ON sender.id = d.sender_source_id \
             WHERE d.source_id = $1 AND d.created_at > now() - interval '10 minutes' \
             ORDER BY d.created_at, d.id LIMIT $2",
        )
        .bind(source)
        .bind(BATCH)
        .fetch_all(&mailbox.pool)
        .await
        .map_err(database("delivery.pending"))?;
        if !pending.is_empty() || tokio::time::timeout_at(deadline, sent).await.is_err() {
            let messages: Vec<_> = pending
                .into_iter()
                .map(|message| {
                    json!({
                        "id": message.id, "session": message.session_id, "text": message.text,
                        "from": message.sender_session_id.map(|session| json!({
                            "session": session, "source": message.sender_name,
                        })),
                    })
                })
                .collect();
            return Ok(Json(json!({ "messages": messages })));
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerMessage {
    session: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentMessage {
    session: String,
    text: String,
    /// The sending session, on the authenticated machine.
    from: String,
}

async fn send_from_owner(
    State(mailbox): State<Mailbox>,
    body: Result<Json<OwnerMessage>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(message) = body.map_err(|_| ApiError::malformed())?;
    send(&mailbox, &message.session, message.text, None).await
}

async fn send_from_agent(
    State(mailbox): State<Mailbox>,
    headers: HeaderMap,
    body: Result<Json<AgentMessage>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let source = auth::authenticate(&mailbox.pool, &headers).await?;
    let Json(message) = body.map_err(|_| ApiError::malformed())?;
    if !valid_id(&message.from) {
        return Err(ApiError::malformed());
    }
    send(
        &mailbox,
        &message.session,
        message.text,
        Some((source, message.from)),
    )
    .await
}

#[derive(FromRow)]
struct Target {
    source_id: Uuid,
    alias: Option<String>,
    session_id: String,
    status: String,
}

async fn send(
    mailbox: &Mailbox,
    session: &str,
    text: String,
    sender: Option<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    if text.trim().is_empty() || text.len() > MAX_TEXT || text.contains('\0') {
        return Err(ApiError::malformed());
    }
    let reference = Reference::parse(session)?;
    let mut sql = QueryBuilder::new(concat!(
        "SELECT s.source_id, sources.alias, s.session_id, ",
        status!(),
        " AS status FROM sessions s JOIN sources ON sources.id = s.source_id \
         WHERE s.deleted_at IS NULL"
    ));
    reference.filter(&mut sql);
    sql.push(" ORDER BY s.source_id LIMIT 11");
    let targets: Vec<Target> = sql
        .build_query_as()
        .fetch_all(&mailbox.pool)
        .await
        .map_err(database("delivery.target"))?;
    let target = reference::one(targets, |target| {
        (
            target.source_id,
            target.alias.as_deref(),
            target.session_id.as_str(),
        )
    })?;
    if target.status == "offline" {
        return Err(ApiError::new(StatusCode::CONFLICT, "session_offline"));
    }
    let id = Uuid::new_v4();
    let (sender_source, sender_session) = sender.unzip();
    sqlx::query(
        "INSERT INTO deliveries (id, source_id, session_id, text, sender_source_id, sender_session_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(target.source_id)
    .bind(&target.session_id)
    .bind(&text)
    .bind(sender_source)
    .bind(sender_session)
    .execute(&mailbox.pool)
    .await
    .map_err(database("delivery.send"))?;
    mailbox.sent.notify_waiters();
    Ok(Json(json!({ "id": id, "status": target.status })))
}
