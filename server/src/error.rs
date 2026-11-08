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
}

impl From<sqlx::Error> for ApiError {
    fn from(_: sqlx::Error) -> Self {
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
