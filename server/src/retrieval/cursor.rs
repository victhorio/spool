use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Find, Search};
use crate::error::ApiError;

// The version and variant/field order are the binary wire format; change the version
// before reordering fields or variants. UUIDs remain internal, encoded as 16 bytes.
const VERSION: u8 = 2;

#[derive(Serialize, Deserialize)]
pub(super) enum Cursor {
    Find {
        query: Find,
        after: FindKey,
    },
    Search {
        source: Uuid,
        session: String,
        query: String,
        revision: i64,
        include_tools: bool,
        limit: Option<u16>,
        before: i32,
    },
    Read {
        source: Uuid,
        session: String,
        revision: i64,
        start: i32,
        end: i32,
        offset: i64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FindKey {
    pub score: i32,
    pub activity: i64,
    pub source: Uuid,
    pub session: String,
}

impl Cursor {
    pub fn encode(&self) -> String {
        let bytes = postcard::to_extend(self, vec![VERSION]).expect("serializable cursor");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    pub fn decode(value: &str) -> Result<Self, ApiError> {
        if value.len() > 8192 {
            return Err(ApiError::malformed());
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| ApiError::malformed())?;
        if bytes.first() == Some(&VERSION) {
            let (cursor, remaining) =
                postcard::take_from_bytes(&bytes[1..]).map_err(|_| ApiError::malformed())?;
            return if remaining.is_empty() {
                Ok(cursor)
            } else {
                Err(ApiError::malformed())
            };
        }
        // Previously issued JSON cursors remain usable across reader upgrades.
        let legacy: Legacy = serde_json::from_slice(&bytes).map_err(|_| ApiError::malformed())?;
        Ok(match legacy {
            Legacy::Find { query, after } => Self::Find { query, after },
            Legacy::Search {
                query,
                revision,
                before,
            } => {
                let (source, session) = legacy_identity(&query.session)?;
                Self::Search {
                    source,
                    session,
                    query: query.query,
                    revision,
                    include_tools: query.include_tools,
                    limit: query.limit,
                    before,
                }
            }
            Legacy::Read {
                session,
                revision,
                start,
                end,
                offset,
            } => {
                let (source, session) = legacy_identity(&session)?;
                Self::Read {
                    source,
                    session,
                    revision,
                    start,
                    end,
                    offset,
                }
            }
        })
    }
}

fn legacy_identity(reference: &str) -> Result<(Uuid, String), ApiError> {
    let (source, session) = reference.split_once('/').ok_or_else(ApiError::malformed)?;
    Ok((
        source.parse().map_err(|_| ApiError::malformed())?,
        session.to_owned(),
    ))
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Legacy {
    Find {
        query: Find,
        after: FindKey,
    },
    Search {
        query: Search,
        revision: i64,
        before: i32,
    },
    Read {
        session: String,
        revision: i64,
        start: i32,
        end: i32,
        offset: i64,
    },
}
