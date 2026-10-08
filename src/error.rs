use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

/// Application error rendered as `{"error":{"code","message","details"}}`.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), details: Value::Null }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "validation", message)
    }
    pub fn unauthenticated() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthenticated", "Please sign in.")
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    /// Also used when the actor may not see the object: never leak existence.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found.")
    }
    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    pub fn invalid_transition(message: impl Into<String>) -> Self {
        Self::conflict("invalid_transition", message)
    }
    pub fn version_conflict(current: Value) -> Self {
        Self::conflict(
            "version_conflict",
            "Someone else changed this record. Review their changes and try again.",
        )
        .with_details(json!({ "current": current }))
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        // Map trigger/constraint aborts raised by the schema to domain errors.
        if let rusqlite::Error::SqliteFailure(_, Some(msg)) = &e {
            match msg.as_str() {
                "hearing_conflict" => {
                    return AppError::conflict(
                        "hearing_conflict",
                        "The judge or the room is already booked for an overlapping time.",
                    );
                }
                "audit_immutable" => return AppError::forbidden("The history cannot be changed."),
                m if m.ends_with("_forbidden") || m.ends_with("_immutable") => {
                    return AppError::forbidden("This record is permanent and cannot be removed or rewritten.");
                }
                m if m.starts_with("UNIQUE constraint failed") => {
                    return AppError::conflict("duplicate", "A record with the same unique value already exists.")
                        .with_details(json!({ "constraint": m }));
                }
                _ => {}
            }
        }
        if let rusqlite::Error::QueryReturnedNoRows = e {
            return AppError::not_found();
        }
        tracing::error!("database error: {e}");
        AppError::internal("Database error.")
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        tracing::error!("io error: {e}");
        AppError::internal("Storage error.")
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::validation(format!("Invalid JSON: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message, "details": self.details } });
        let mut res = (self.status, axum::Json(body)).into_response();
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, private"));
        res
    }
}
