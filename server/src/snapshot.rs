use std::{
    collections::{HashMap, HashSet},
    fmt,
    marker::PhantomData,
};

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::error::ApiError;

const MAX_ITEMS: usize = 100_000;
const MAX_ARCHIVE: usize = 16 * 1024 * 1024;
pub(crate) const MAX_BODY: usize = 64 * 1024 * 1024;

fn check_size(body: &[u8]) -> Result<(), ApiError> {
    if body.len() <= MAX_ARCHIVE {
        return Ok(());
    }
    struct ArchiveSize<'a>(&'a [u8]);

    impl<'de> Visitor<'de> for ArchiveSize<'_> {
        type Value = usize;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a snapshot object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<usize, A::Error> {
            let mut previous_end = None;
            let mut removed = 0;
            while let Some(key) = map.next_key::<String>()? {
                let value: &'de RawValue = map.next_value()?;
                let end =
                    value.get().as_ptr() as usize - self.0.as_ptr() as usize + value.get().len();
                if key == "retrieval" {
                    if removed != 0 {
                        return Err(de::Error::duplicate_field("retrieval"));
                    }
                    // Remove the whole member, not just its value: an archive at
                    // exactly 16 MiB must still fit when a projection is added.
                    removed = if let Some(previous) = previous_end {
                        end - previous
                    } else {
                        let start = self.0.iter().position(|byte| *byte == b'{').unwrap() + 1;
                        let comma = self.0[end..]
                            .iter()
                            .position(|byte| !byte.is_ascii_whitespace())
                            .filter(|offset| self.0[end + offset] == b',');
                        end - start + comma.map_or(0, |offset| offset + 1)
                    };
                }
                previous_end = Some(end);
            }
            Ok(self.0.len() - removed)
        }
    }

    let oversized = || ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    if body.len() > MAX_BODY {
        return Err(oversized());
    }
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let size = deserializer
        .deserialize_map(ArchiveSize(body))
        .map_err(|_| oversized())?;
    deserializer.end().map_err(|_| oversized())?;
    if size > MAX_ARCHIVE {
        return Err(oversized());
    }
    Ok(())
}

fn bounded_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Limited<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Limited<T> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            write!(formatter, "an array of at most {MAX_ITEMS} entries")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Vec<T>, A::Error> {
            let mut values = Vec::new();
            while values.len() < MAX_ITEMS {
                match sequence.next_element()? {
                    Some(value) => values.push(value),
                    None => return Ok(values),
                }
            }
            if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("array limit exceeded"));
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(Limited(PhantomData))
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    base_revision: Option<String>,
    format: String,
    upstream_version: String,
    collector_version: String,
    captured_at: DateTime<Utc>,
    info: Box<RawValue>,
    #[serde(deserialize_with = "bounded_list")]
    message_order: Vec<String>,
    #[serde(deserialize_with = "bounded_list")]
    upsert_messages: Vec<Box<RawValue>>,
    #[serde(default, deserialize_with = "retrieval_value")]
    retrieval: Option<Box<RawValue>>,
}

fn retrieval_value<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct Retrieval {
    version: u8,
    title: String,
    #[serde(deserialize_with = "Option::<String>::deserialize")]
    repository: Option<String>,
    activity: f64,
    #[serde(deserialize_with = "bounded_list")]
    messages: Vec<Projection>,
}

#[derive(Deserialize)]
struct Projection {
    id: String,
    role: String,
    text: String,
    conversation: String,
    tools: String,
    #[serde(deserialize_with = "bounded_list")]
    conversation_spans: Vec<[usize; 2]>,
    #[serde(deserialize_with = "bounded_list")]
    tool_spans: Vec<[usize; 2]>,
}

fn valid_spans(text: &str, searchable: &str, spans: &[[usize; 2]]) -> bool {
    let mut previous_end = 0;
    let mut remaining = searchable;
    for (index, &[start, end]) in spans.iter().enumerate() {
        if start < previous_end {
            return false;
        }
        let Some(selected) = text.get(start..end) else {
            return false;
        };
        if index > 0 {
            let Some(after_separator) = remaining.strip_prefix('\n') else {
                return false;
            };
            remaining = after_separator;
        }
        let Some(after_selected) = remaining.strip_prefix(selected) else {
            return false;
        };
        remaining = after_selected;
        previous_end = end;
    }
    remaining.is_empty()
}

#[derive(Deserialize)]
struct ObjectId {
    id: String,
}

impl ObjectId {
    fn parse(document: &RawValue) -> Result<Self, ApiError> {
        if !document.get().starts_with('{') {
            return Err(ApiError::manifest());
        }
        serde_json::from_str(document.get()).map_err(|_| ApiError::manifest())
    }
}

struct Message {
    payload: Box<RawValue>,
    hash: Vec<u8>,
}

pub(crate) struct Snapshot {
    envelope: Envelope,
    retrieval: Option<Retrieval>,
    base: Option<i64>,
    info_hash: Vec<u8>,
    messages: HashMap<String, Message>,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.contains('\0')
}

impl Snapshot {
    pub fn parse(body: &[u8], session_id: &str) -> Result<Self, ApiError> {
        check_size(body)?;
        if body.trim_ascii_start().first() != Some(&b'{') {
            return Err(ApiError::malformed());
        }
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
        let info = ObjectId::parse(&envelope.info)?;
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
            let message = ObjectId::parse(&payload)?;
            if !order.contains(message.id.as_str()) || messages.contains_key(&message.id) {
                return Err(ApiError::manifest());
            }
            let hash = Sha256::digest(payload.get().as_bytes()).to_vec();
            messages.insert(message.id, Message { payload, hash });
        }
        let retrieval = envelope
            .retrieval
            .take()
            .map(|raw| {
                if !raw.get().starts_with('{') {
                    return Err(ApiError::malformed());
                }
                let retrieval: Retrieval =
                    serde_json::from_str(raw.get()).map_err(|_| ApiError::malformed())?;
                if retrieval.version != 1
                    || retrieval.title.len() > 4096
                    || retrieval.title.contains('\0')
                    || retrieval
                        .repository
                        .as_ref()
                        .is_some_and(|value| value.len() > 1024 || value.contains('\0'))
                    || !retrieval.activity.is_finite()
                    || retrieval.activity.fract() != 0.0
                    || retrieval.activity.abs() > 8.64e15
                {
                    return Err(ApiError::manifest());
                }
                let mut projected = HashSet::with_capacity(retrieval.messages.len());
                for projection in &retrieval.messages {
                    if !valid_id(&projection.id)
                        || !messages.contains_key(&projection.id)
                        || !projected.insert(projection.id.as_str())
                        || projection.role.len() > 256
                        || [
                            &projection.role,
                            &projection.text,
                            &projection.conversation,
                            &projection.tools,
                        ]
                        .iter()
                        .any(|value| value.contains('\0'))
                        || !valid_spans(
                            &projection.text,
                            &projection.conversation,
                            &projection.conversation_spans,
                        )
                        || !valid_spans(&projection.text, &projection.tools, &projection.tool_spans)
                    {
                        return Err(ApiError::manifest());
                    }
                }
                if projected.len() != messages.len() {
                    return Err(ApiError::manifest());
                }
                Ok(retrieval)
            })
            .transpose()?;
        let info_hash = Sha256::digest(envelope.info.get().as_bytes()).to_vec();
        Ok(Self {
            envelope,
            retrieval,
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
    let mut tx = pool
        .begin()
        .await
        .map_err(|error| ApiError::database("snapshot.begin", error))?;
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
        .await.map_err(|error| ApiError::database("snapshot.create", error))?
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
    .await
    .map_err(|error| ApiError::database("snapshot.lock", error))?;
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
    let rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT message_id, position, payload_hash FROM messages WHERE source_id = $1 AND session_id = $2",
    )
    .bind(source)
    .bind(session)
    .fetch_all(&mut *tx)
    .await.map_err(|error| ApiError::database("snapshot.messages", error))?;
    let mut existing: HashMap<_, _> = rows
        .iter()
        .map(|message| (message.message_id.as_str(), message))
        .collect();
    let mut ids = Vec::new();
    let mut positions = Vec::new();
    let mut payloads = Vec::new();
    let mut hashes = Vec::new();
    let mut projections: HashMap<_, _> = snapshot
        .retrieval
        .iter()
        .flat_map(|retrieval| &retrieval.messages)
        .map(|message| (message.id.as_str(), message))
        .collect();
    let mut roles = Vec::new();
    let mut texts = Vec::new();
    let mut conversations = Vec::new();
    let mut tools = Vec::new();
    let mut conversation_spans = Vec::new();
    let mut tool_spans = Vec::new();
    let mut moved_ids = Vec::new();
    let mut moved_positions = Vec::new();
    let mut changed = created
        || current.info_hash.as_ref() != Some(&snapshot.info_hash)
        || existing.len() != envelope.message_order.len();
    for (position, id) in envelope.message_order.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| ApiError::manifest())?;
        let old = existing.remove(id.as_str());
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
            let projection = projections.remove(id.as_str());
            roles.push(projection.map(|message| message.role.as_str()));
            texts.push(projection.map(|message| message.text.as_str()));
            conversations.push(projection.map(|message| message.conversation.as_str()));
            tools.push(projection.map(|message| message.tools.as_str()));
            conversation_spans.push(projection.map(|message| {
                serde_json::to_string(&message.conversation_spans).expect("spans are serializable")
            }));
            tool_spans.push(projection.map(|message| {
                serde_json::to_string(&message.tool_spans).expect("spans are serializable")
            }));
            changed = true;
        } else if old.is_some_and(|old| old.position != position) {
            moved_ids.push(id.as_str());
            moved_positions.push(position);
            changed = true;
        }
    }
    // Resolve every manifest reference before any destructive write. The deferred
    // position constraint permits swaps without temporary positions or rewrites.
    if !existing.is_empty() {
        let removed: Vec<_> = existing.into_keys().collect();
        sqlx::query(
            "DELETE FROM messages USING unnest($3::text[]) AS removed(id) \
             WHERE source_id = $1 AND session_id = $2 AND message_id = removed.id",
        )
        .bind(source)
        .bind(session)
        .bind(&removed)
        .execute(&mut *tx)
        .await
        .map_err(|error| ApiError::database("snapshot.delete", error))?;
    }
    if !ids.is_empty() {
        sqlx::query(
            "INSERT INTO messages (source_id, session_id, message_id, position, payload, payload_hash, \
             retrieval_role, retrieval_text, conversation_text, tool_text, conversation_spans, tool_spans) \
             SELECT $1, $2, batch.id, batch.position, batch.payload::json, batch.hash, \
             batch.role, batch.text, batch.conversation, batch.tools, \
             batch.conversation_spans::jsonb, batch.tool_spans::jsonb \
             FROM unnest($3::text[], $4::integer[], $5::text[], $6::bytea[], \
             $7::text[], $8::text[], $9::text[], $10::text[], $11::text[], $12::text[]) \
             AS batch(id, position, payload, hash, role, text, conversation, tools, conversation_spans, tool_spans) \
             ON CONFLICT (source_id, session_id, message_id) DO UPDATE \
             SET position = EXCLUDED.position, payload = EXCLUDED.payload, payload_hash = EXCLUDED.payload_hash, \
             retrieval_role = EXCLUDED.retrieval_role, retrieval_text = EXCLUDED.retrieval_text, \
             conversation_text = EXCLUDED.conversation_text, tool_text = EXCLUDED.tool_text, \
             conversation_spans = EXCLUDED.conversation_spans, tool_spans = EXCLUDED.tool_spans",
        )
        .bind(source)
        .bind(session)
        .bind(&ids)
        .bind(&positions)
        .bind(&payloads)
        .bind(&hashes)
        .bind(&roles)
        .bind(&texts)
        .bind(&conversations)
        .bind(&tools)
        .bind(&conversation_spans)
        .bind(&tool_spans)
        .execute(&mut *tx)
        .await.map_err(|error| ApiError::database("snapshot.upsert", error))?;
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
        .await
        .map_err(|error| ApiError::database("snapshot.reorder", error))?;
    }
    if !projections.is_empty() {
        let mut ids = Vec::new();
        let mut roles = Vec::new();
        let mut texts = Vec::new();
        let mut conversations = Vec::new();
        let mut tools = Vec::new();
        let mut conversation_spans = Vec::new();
        let mut tool_spans = Vec::new();
        for message in projections.into_values() {
            ids.push(&message.id);
            roles.push(&message.role);
            texts.push(&message.text);
            conversations.push(&message.conversation);
            tools.push(&message.tools);
            conversation_spans.push(
                serde_json::to_string(&message.conversation_spans).expect("spans are serializable"),
            );
            tool_spans
                .push(serde_json::to_string(&message.tool_spans).expect("spans are serializable"));
        }
        sqlx::query(
            "UPDATE messages SET retrieval_role = batch.role, retrieval_text = batch.text, \
             conversation_text = batch.conversation, tool_text = batch.tools, \
             conversation_spans = batch.conversation_spans::jsonb, tool_spans = batch.tool_spans::jsonb \
             FROM unnest($3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::text[], $9::text[]) \
             AS batch(id, role, text, conversation, tools, conversation_spans, tool_spans) \
             WHERE source_id = $1 AND session_id = $2 AND message_id = batch.id \
             AND (retrieval_role, retrieval_text, conversation_text, tool_text, \
             messages.conversation_spans, messages.tool_spans) \
             IS DISTINCT FROM (batch.role, batch.text, batch.conversation, batch.tools, \
             batch.conversation_spans::jsonb, batch.tool_spans::jsonb)",
        )
        .bind(source)
        .bind(session)
        .bind(&ids)
        .bind(&roles)
        .bind(&texts)
        .bind(&conversations)
        .bind(&tools)
        .bind(&conversation_spans)
        .bind(&tool_spans)
        .execute(&mut *tx)
        .await
        .map_err(|error| ApiError::database("snapshot.project", error))?;
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
        .await.map_err(|error| ApiError::database("snapshot.update", error))?;
    }
    let retrieval = snapshot.retrieval.as_ref();
    sqlx::query(
        "UPDATE sessions SET retrieval_title = CASE WHEN NOT EXISTS \
         (SELECT 1 FROM messages WHERE source_id = $1 AND session_id = $2 AND \
         (retrieval_role IS NULL OR retrieval_text IS NULL OR conversation_text IS NULL OR tool_text IS NULL \
         OR conversation_spans IS NULL OR tool_spans IS NULL)) \
         THEN $3 ELSE NULL END, repository = $4, activity = $5 \
         WHERE source_id = $1 AND session_id = $2",
    )
    .bind(source)
    .bind(session)
    .bind(retrieval.map(|retrieval| retrieval.title.as_str()))
    .bind(retrieval.and_then(|retrieval| retrieval.repository.as_deref()))
    .bind(retrieval.map(|retrieval| retrieval.activity as i64))
    .execute(&mut *tx)
    .await
    .map_err(|error| ApiError::database("snapshot.metadata", error))?;
    tx.commit()
        .await
        .map_err(|error| ApiError::database("snapshot.commit", error))?;
    Ok(Accepted {
        revision,
        changed,
        created,
    })
}
