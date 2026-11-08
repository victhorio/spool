use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::ApiError;

#[derive(Deserialize)]
struct Envelope {
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    base_revision: Option<String>,
    format: String,
    upstream_version: String,
    collector_version: String,
    captured_at: DateTime<Utc>,
    info: Box<RawValue>,
    message_order: Vec<String>,
    upsert_messages: Vec<Box<RawValue>>,
}

#[derive(Deserialize)]
struct ObjectId {
    id: String,
}

struct Message {
    payload: Box<RawValue>,
    hash: Vec<u8>,
}

pub(crate) struct Snapshot {
    envelope: Envelope,
    base: Option<i64>,
    info_hash: Vec<u8>,
    messages: HashMap<String, Message>,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.contains('\0')
}

impl Snapshot {
    pub fn parse(body: &[u8], session_id: &str) -> Result<Self, ApiError> {
        let mut envelope: Envelope =
            serde_json::from_slice(body).map_err(|_| ApiError::malformed())?;
        if !valid_id(session_id)
            || envelope.format != "opencode.v2"
            || [&envelope.upstream_version, &envelope.collector_version]
                .iter()
                .any(|value| value.is_empty() || value.len() > 256 || value.contains('\0'))
        {
            return Err(ApiError::malformed());
        }
        let base = envelope
            .base_revision
            .as_deref()
            .map(|value| {
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|revision| *revision > 0)
                    .ok_or_else(ApiError::malformed)
            })
            .transpose()?;
        let info: ObjectId =
            serde_json::from_str(envelope.info.get()).map_err(|_| ApiError::manifest())?;
        if info.id != session_id {
            return Err(ApiError::manifest());
        }
        let mut order = HashSet::with_capacity(envelope.message_order.len());
        for id in &envelope.message_order {
            if !valid_id(id) || !order.insert(id.as_str()) {
                return Err(ApiError::manifest());
            }
        }
        let mut messages = HashMap::with_capacity(envelope.upsert_messages.len());
        for payload in std::mem::take(&mut envelope.upsert_messages) {
            let message: ObjectId =
                serde_json::from_str(payload.get()).map_err(|_| ApiError::manifest())?;
            if !order.contains(message.id.as_str()) || messages.contains_key(&message.id) {
                return Err(ApiError::manifest());
            }
            let hash = Sha256::digest(payload.get().as_bytes()).to_vec();
            messages.insert(message.id, Message { payload, hash });
        }
        let info_hash = Sha256::digest(envelope.info.get().as_bytes()).to_vec();
        Ok(Self {
            envelope,
            base,
            info_hash,
            messages,
        })
    }
}

#[derive(FromRow)]
struct SessionRow {
    revision: i64,
    deleted_at: Option<DateTime<Utc>>,
    info_hash: Option<Vec<u8>>,
}

#[derive(FromRow)]
struct MessageRow {
    message_id: String,
    position: i32,
    payload_hash: Vec<u8>,
}

pub(crate) struct Accepted {
    pub revision: i64,
    pub changed: bool,
    pub created: bool,
}

pub(crate) async fn accept(
    pool: &PgPool,
    source: Uuid,
    session: &str,
    snapshot: Snapshot,
) -> Result<Accepted, ApiError> {
    let envelope = &snapshot.envelope;
    let mut tx = pool.begin().await?;
    let created = if snapshot.base.is_none() {
        sqlx::query(
            "INSERT INTO sessions \
             (source_id, session_id, revision, format, info, info_hash, upstream_version, collector_version, captured_at) \
             VALUES ($1, $2, 1, 'opencode.v2', $3::json, $4, $5, $6, $7) \
             ON CONFLICT (source_id, session_id) DO NOTHING",
        )
        .bind(source)
        .bind(session)
        .bind(envelope.info.get())
        .bind(&snapshot.info_hash)
        .bind(&envelope.upstream_version)
        .bind(&envelope.collector_version)
        .bind(envelope.captured_at)
        .execute(&mut *tx)
        .await?
        .rows_affected() == 1
    } else {
        false
    };
    let current: Option<SessionRow> = sqlx::query_as(
        "SELECT revision, deleted_at, info_hash FROM sessions \
         WHERE source_id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(source)
    .bind(session)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        return Err(ApiError::conflict(None));
    };
    if current.deleted_at.is_some() {
        return Err(ApiError::new(StatusCode::GONE, "session_deleted"));
    }
    if !created && snapshot.base != Some(current.revision) {
        return Err(ApiError::conflict(Some(current.revision)));
    }
    let revision = if created {
        1
    } else {
        current
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "revision_exhausted"))?
    };
    let existing: Vec<MessageRow> = sqlx::query_as(
        "SELECT message_id, position, payload_hash FROM messages WHERE source_id = $1 AND session_id = $2",
    )
    .bind(source)
    .bind(session)
    .fetch_all(&mut *tx)
    .await?;
    let existing: HashMap<_, _> = existing
        .into_iter()
        .map(|message| (message.message_id.clone(), message))
        .collect();
    let mut ids = Vec::new();
    let mut positions = Vec::new();
    let mut payloads = Vec::new();
    let mut hashes = Vec::new();
    let mut moved_ids = Vec::new();
    let mut moved_positions = Vec::new();
    let mut changed = created
        || current.info_hash.as_ref() != Some(&snapshot.info_hash)
        || existing.len() != envelope.message_order.len();
    for (position, id) in envelope.message_order.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| ApiError::manifest())?;
        let old = existing.get(id);
        let supplied = snapshot.messages.get(id);
        if old.is_none() && supplied.is_none() {
            return Err(ApiError::manifest());
        }
        if let Some(message) = supplied
            && old.is_none_or(|old| old.payload_hash != message.hash)
        {
            ids.push(id.as_str());
            positions.push(position);
            payloads.push(message.payload.get());
            hashes.push(message.hash.as_slice());
            changed = true;
        } else if old.is_some_and(|old| old.position != position) {
            moved_ids.push(id.as_str());
            moved_positions.push(position);
            changed = true;
        }
    }
    // Resolve every manifest reference before any destructive write. The deferred
    // position constraint permits swaps without temporary positions or rewrites.
    sqlx::query(
        "DELETE FROM messages WHERE source_id = $1 AND session_id = $2 AND NOT (message_id = ANY($3::text[]))",
    )
    .bind(source)
    .bind(session)
    .bind(&envelope.message_order)
    .execute(&mut *tx)
    .await?;
    if !ids.is_empty() {
        sqlx::query(
            "INSERT INTO messages (source_id, session_id, message_id, position, payload, payload_hash) \
             SELECT $1, $2, batch.id, batch.position, batch.payload::json, batch.hash \
             FROM unnest($3::text[], $4::integer[], $5::text[], $6::bytea[]) AS batch(id, position, payload, hash) \
             ON CONFLICT (source_id, session_id, message_id) DO UPDATE \
             SET position = EXCLUDED.position, payload = EXCLUDED.payload, payload_hash = EXCLUDED.payload_hash",
        )
        .bind(source)
        .bind(session)
        .bind(&ids)
        .bind(&positions)
        .bind(&payloads)
        .bind(&hashes)
        .execute(&mut *tx)
        .await?;
    }
    if !moved_ids.is_empty() {
        sqlx::query(
            "UPDATE messages SET position = batch.position \
             FROM unnest($3::text[], $4::integer[]) AS batch(id, position) \
             WHERE source_id = $1 AND session_id = $2 AND message_id = batch.id",
        )
        .bind(source)
        .bind(session)
        .bind(&moved_ids)
        .bind(&moved_positions)
        .execute(&mut *tx)
        .await?;
    }
    if !created {
        sqlx::query(
            "UPDATE sessions SET revision = $3, \
             info = CASE WHEN info_hash IS DISTINCT FROM $5 THEN $4::json ELSE info END, \
             info_hash = $5, upstream_version = $6, collector_version = $7, captured_at = $8, received_at = now() \
             WHERE source_id = $1 AND session_id = $2",
        )
        .bind(source)
        .bind(session)
        .bind(revision)
        .bind(envelope.info.get())
        .bind(&snapshot.info_hash)
        .bind(&envelope.upstream_version)
        .bind(&envelope.collector_version)
        .bind(envelope.captured_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Accepted {
        revision,
        changed,
        created,
    })
}
