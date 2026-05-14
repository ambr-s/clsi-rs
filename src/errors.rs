use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

/// HTTP error types web hard-codes branches for (see ClsiManager._postToClsi):
///   413 → project-too-large
///   423 → compile-in-progress
///   502/503 → unavailable
///   504 → timedout
/// We add a generic 400 / 500 for the obvious cases.
#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    BadRequest(String),
    Conflict,           // 409 — incremental sync mismatch (we never emit for v1)
    Locked,             // 423 — compile already in progress for this scope
    Timeout,            // 504
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ApiError::Conflict => (
                StatusCode::CONFLICT,
                Json(json!({"compile": {"status": "conflict"}})),
            )
                .into_response(),
            ApiError::Locked => StatusCode::LOCKED.into_response(),
            ApiError::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({"compile": {"status": "timedout"}})),
            )
                .into_response(),
            ApiError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"compile": {"status": "error", "error": e.to_string()}})),
                )
                    .into_response()
            }
        }
    }
}
