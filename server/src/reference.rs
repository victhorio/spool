//! Session references, shared by the agent tools and the reader's links: a
//! native session ID, qualified as `alias/ID` (or `source-UUID/ID`) only when
//! another source archives the same ID.

use axum::http::StatusCode;
use sqlx::{Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{error::ApiError, snapshot::valid_id};

pub(crate) struct Reference<'a> {
    qualifier: Option<Qualifier<'a>>,
    id: &'a str,
}

enum Qualifier<'a> {
    Source(Uuid),
    Alias(&'a str),
}

impl<'a> Reference<'a> {
    pub fn parse(input: &'a str) -> Result<Self, ApiError> {
        let (qualifier, id) = match input.split_once('/') {
            Some((qualifier, id)) => (Some(qualifier), id),
            None => (None, input),
        };
        if !valid_id(id) {
            return Err(ApiError::malformed());
        }
        let qualifier = match qualifier {
            None => None,
            Some(source) if source.len() == 36 => Some(Qualifier::Source(
                source.parse().map_err(|_| ApiError::malformed())?,
            )),
            Some(alias)
                if alias.len() <= 32
                    && alias.starts_with(|c: char| c.is_ascii_lowercase())
                    && alias
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') =>
            {
                Some(Qualifier::Alias(alias))
            }
            Some(_) => return Err(ApiError::malformed()),
        };
        Ok(Self { qualifier, id })
    }

    /// Restricts a query over `sessions s JOIN sources` to the referenced sessions.
    pub fn filter(&self, sql: &mut QueryBuilder<'a, Postgres>) {
        sql.push(" AND s.session_id = ").push_bind(self.id);
        match self.qualifier {
            Some(Qualifier::Source(source)) => sql.push(" AND s.source_id = ").push_bind(source),
            Some(Qualifier::Alias(alias)) => sql.push(" AND sources.alias = ").push_bind(alias),
            None => sql,
        };
    }
}

/// SQL for whether another live source archives the session `table.session_id`
/// from `table.source_id`, which is when its reference needs a qualifier.
macro_rules! shared {
    ($table:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM sessions other WHERE other.session_id = ",
            $table,
            ".session_id AND other.source_id <> ",
            $table,
            ".source_id AND other.deleted_at IS NULL)"
        )
    };
}
pub(crate) use shared;

/// The reader's link to a session, or to one of its one-based message indexes,
/// such as `https://spool.example/sessions/ses_…#12`.
pub(crate) fn link(reader: &str, reference: &str, index: Option<i32>) -> String {
    let mut url = format!("{reader}/sessions/");
    for byte in reference.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(char::from(byte))
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    if let Some(index) = index {
        url.push_str(&format!("#{index}"));
    }
    url
}

pub(crate) fn format(source: Uuid, alias: Option<&str>, session: &str, shared: bool) -> String {
    match (shared, alias) {
        (false, _) => session.to_owned(),
        (true, Some(alias)) => format!("{alias}/{session}"),
        (true, None) => format!("{source}/{session}"),
    }
}

/// The one session a reference names, from at most 11 matching rows ordered by
/// source; several matches return their qualified references instead.
pub(crate) fn one<T>(
    mut rows: Vec<T>,
    identity: impl Fn(&T) -> (Uuid, Option<&str>, &str),
) -> Result<T, ApiError> {
    if rows.len() > 1 {
        let candidates = rows
            .iter()
            .take(10)
            .map(|row| {
                let (source, alias, session) = identity(row);
                format(source, alias, session, true)
            })
            .collect();
        return Err(ApiError::ambiguous_session(candidates, rows.len() > 10));
    }
    rows.pop()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found"))
}
