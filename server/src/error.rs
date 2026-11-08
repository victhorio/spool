use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    revision: Option<Option<i64>>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            revision: None,
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
            status: StatusCode::CONFLICT,
            code: "revision_conflict",
            revision: Some(current),
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
        if let Some(revision) = self.revision {
            error["current_revision"] = json!(revision.map(|value| value.to_string()));
        }
        (self.status, Json(json!({ "error": error }))).into_response()
    }
}
