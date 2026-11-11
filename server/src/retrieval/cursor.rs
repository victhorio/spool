use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Find;

/// Continues an agent tool result; see [`crate::page`] for the encoding.
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
pub(super) struct FindKey {
    pub score: i32,
    pub activity: i64,
    pub source: Uuid,
    pub session: String,
}
