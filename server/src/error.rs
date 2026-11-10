use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

use crate::retrieval::PAGE_BYTES;

pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    revision: Option<Option<i64>>,
    candidates: Option<(Vec<String>, bool)>,
    edit: Option<usize>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            revision: None,
            candidates: None,
            edit: None,
        }
    }

    /// A failed artifact edit, with the zero-based index of the edit.
    pub fn edit(code: &'static str, index: usize) -> Self {
        Self {
            edit: Some(index),
            ..Self::new(StatusCode::UNPROCESSABLE_ENTITY, code)
        }
    }

    pub fn malformed() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "malformed_request")
    }

    pub fn manifest() -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_manifest")
    }

    pub fn conflict(current: Option<i64>) -> Self {
        Self {
            revision: Some(current),
            ..Self::new(StatusCode::CONFLICT, "revision_conflict")
        }
    }

    pub fn session_changed(current: i64) -> Self {
        Self {
            revision: Some(Some(current)),
            ..Self::new(StatusCode::CONFLICT, "session_changed")
        }
    }

    pub fn ambiguous_session(candidates: Vec<String>, more: bool) -> Self {
        Self {
            candidates: Some((candidates, more)),
            ..Self::new(StatusCode::CONFLICT, "ambiguous_session")
        }
    }

    pub fn database(operation: &'static str, error: sqlx::Error) -> Self {
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

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut error = json!({ "code": self.code });
        if let Some(index) = self.edit {
            error["edit"] = json!(index);
        }
        if let Some(revision) = self.revision {
            error["current_revision"] = json!(revision.map(|value| value.to_string()));
        }
        let mut body = json!({ "error": error });
        if let Some((candidates, more)) = self.candidates {
            body["error"]["candidates"] = json!(candidates);
            body["error"]["more"] = json!(more);
            while serde_json::to_vec(&body).expect("serializable error").len() > PAGE_BYTES {
                body["error"]["candidates"]
                    .as_array_mut()
                    .expect("ambiguity candidates")
                    .pop()
                    .expect("bounded individual candidate");
                body["error"]["more"] = json!(true);
            }
        }
        (self.status, Json(body)).into_response()
    }
}
