use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::PAGE_BYTES;

pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: Detail,
}

/// What an error adds to its code.
enum Detail {
    None,
    /// The current revision, or none when the record does not exist yet.
    Revision(Option<i64>),
    /// Qualified references, and whether more matched than are listed.
    Candidates(Vec<String>, bool),
    /// The zero-based index of the artifact edit that failed.
    Edit(usize),
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            detail: Detail::None,
        }
    }

    pub fn edit(code: &'static str, index: usize) -> Self {
        Self {
            detail: Detail::Edit(index),
            ..Self::new(StatusCode::UNPROCESSABLE_ENTITY, code)
        }
    }

    pub fn malformed() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "malformed_request")
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found")
    }

    pub fn too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
    }

    pub fn manifest() -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_manifest")
    }

    pub fn conflict(current: Option<i64>) -> Self {
        Self {
            detail: Detail::Revision(current),
            ..Self::new(StatusCode::CONFLICT, "revision_conflict")
        }
    }

    pub fn session_changed(current: i64) -> Self {
        Self {
            detail: Detail::Revision(Some(current)),
            ..Self::new(StatusCode::CONFLICT, "session_changed")
        }
    }

    pub fn ambiguous_session(candidates: Vec<String>, more: bool) -> Self {
        Self {
            detail: Detail::Candidates(candidates, more),
            ..Self::new(StatusCode::CONFLICT, "ambiguous_session")
        }
    }

    /// Logs a database failure without its message, which may contain data.
    pub fn database(operation: &'static str) -> impl FnOnce(sqlx::Error) -> Self {
        move |error| {
            let class = match &error {
                sqlx::Error::Database(_) => "database",
                sqlx::Error::Io(_) => "io",
                sqlx::Error::Tls(_) => "tls",
                sqlx::Error::PoolTimedOut => "pool_timeout",
                sqlx::Error::PoolClosed => "pool_closed",
                sqlx::Error::Protocol(_) => "protocol",
                _ => "client",
            };
            let sqlstate = error.as_database_error().and_then(|error| error.code());
            eprintln!(
                "{}",
                json!({
                    "event": "database_error", "operation": operation,
                    "class": class, "sqlstate": sqlstate,
                })
            );
            Self::new(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut body = json!({ "error": { "code": self.code } });
        let error = &mut body["error"];
        match self.detail {
            Detail::None => {}
            Detail::Revision(revision) => {
                error["current_revision"] = json!(revision.map(|value| value.to_string()));
            }
            Detail::Edit(index) => error["edit"] = json!(index),
            Detail::Candidates(candidates, more) => {
                error["candidates"] = json!(candidates);
                error["more"] = json!(more);
                // Agents read this as a tool result, which must fit in one page.
                while serde_json::to_vec(&body).expect("serializable error").len() > PAGE_BYTES {
                    body["error"]["candidates"]
                        .as_array_mut()
                        .expect("ambiguity candidates")
                        .pop()
                        .expect("bounded individual candidate");
                    body["error"]["more"] = json!(true);
                }
            }
        }
        (self.status, Json(body)).into_response()
    }
}
