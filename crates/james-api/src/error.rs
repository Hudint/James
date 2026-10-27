use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use james_db::DbError;
use serde_json::json;

/// API error rendered as `{"error": "..."}` with a matching status code.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentication required")
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "permission denied")
    }
    pub fn not_found(what: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::NotFound => Self::new(StatusCode::NOT_FOUND, "not found"),
            DbError::Conflict => Self::conflict("already exists"),
            DbError::Invalid(message) => Self::bad_request(message),
            other => {
                tracing::error!(error = %other, "database error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        }
    }
}

/// Errors from the runtime (resolving tracks, platform commands) are shown to the user,
/// shortened to their first line so tool output and internal paths do not leak.
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let full = format!("{e:#}");
        tracing::debug!(error = %full, "request failed");
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, user_message(&full))
    }
}

pub use james_core::text::user_message;
