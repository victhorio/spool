//! Session references, shared by the agent tools and the reader's links: a
//! native session ID, qualified as `alias/ID` (or `source-UUID/ID`) only when
//! another source archives the same ID.

use sqlx::{FromRow, PgExecutor, Postgres, QueryBuilder, postgres::PgRow};
use uuid::Uuid;

use crate::error::ApiError;

/// A native session ID, as OpenCode assigns it.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 512 && !id.contains('\0')
}

/// A source alias, such as `work-m5`; the database enforces the same rule.
pub(crate) fn valid_alias(alias: &str) -> bool {
    (1..=32).contains(&alias.len())
        && alias.as_bytes()[0].is_ascii_lowercase()
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

pub(crate) struct Reference<'a> {
    qualifier: Option<Qualifier<'a>>,
    id: &'a str,
}

enum Qualifier<'a> {
    Source(Uuid),
    Alias(&'a str),
}

/// Archived sessions that were not purged, as `s`, with their source as `sources`.
pub(crate) const LIVE_SESSIONS: &str =
    " FROM sessions s JOIN sources ON sources.id = s.source_id WHERE s.deleted_at IS NULL";

/// A row for one session, which says which session it is.
pub(crate) trait Named {
    /// The session's source, the source's alias, and the session's native ID.
    fn name(&self) -> (Uuid, Option<&str>, &str);
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
            Some(alias) if valid_alias(alias) => Some(Qualifier::Alias(alias)),
            Some(_) => return Err(ApiError::malformed()),
        };
        Ok(Self { qualifier, id })
    }

    /// The session `id` from `source`, such as one a cursor names.
    pub fn exact(source: Uuid, id: &'a str) -> Result<Self, ApiError> {
        if !valid_id(id) {
            return Err(ApiError::malformed());
        }
        Ok(Self {
            qualifier: Some(Qualifier::Source(source)),
            id,
        })
    }

    /// The one live session this reference names, as `columns` selects it from
    /// [`LIVE_SESSIONS`]. Several matches return their qualified references.
    pub async fn resolve<T>(
        &self,
        columns: &str,
        executor: impl PgExecutor<'_>,
    ) -> Result<T, ApiError>
    where
        T: for<'r> FromRow<'r, PgRow> + Named + Send + Unpin,
    {
        let mut sql = QueryBuilder::<Postgres>::new(columns);
        sql.push(LIVE_SESSIONS)
            .push(" AND s.session_id = ")
            .push_bind(self.id);
        match self.qualifier {
            Some(Qualifier::Source(source)) => sql.push(" AND s.source_id = ").push_bind(source),
            Some(Qualifier::Alias(alias)) => sql.push(" AND sources.alias = ").push_bind(alias),
            None => &mut sql,
        };
        // One row more than the candidates listed shows whether there are more.
        sql.push(" ORDER BY s.source_id LIMIT 11");
        let mut rows: Vec<T> = sql
            .build_query_as()
            .fetch_all(executor)
            .await
            .map_err(ApiError::database("session.resolve"))?;
        if rows.len() > 1 {
            let candidates = rows
                .iter()
                .take(10)
                .map(|row| {
                    let (source, alias, session) = row.name();
                    format(source, alias, session, true)
                })
                .collect();
            return Err(ApiError::ambiguous_session(candidates, rows.len() > 10));
        }
        rows.pop().ok_or_else(ApiError::not_found)
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
