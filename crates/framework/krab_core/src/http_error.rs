use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Validation,
    /// The request carries no acceptable credentials (HTTP 401).
    Unauthenticated,
    Authz,
    NotFound,
    Conflict,
    /// The caller exceeded a rate limit (HTTP 429).
    RateLimited,
    Internal,
}

impl ErrorCategory {
    pub fn default_status(&self) -> StatusCode {
        match self {
            Self::Validation => StatusCode::BAD_REQUEST,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Authz => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub category: ErrorCategory,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl ApiError {
    pub fn new(
        category: ErrorCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code: code.into(),
            message: message.into(),
            details: None,
            request_id: None,
            trace_id: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Status comes from the category alone. The magic-string overrides on
        // `code == "UNAUTHORIZED"` / `code == "TOO_MANY_REQUESTS"` are gone:
        // use `ErrorCategory::Unauthenticated` (401) and
        // `ErrorCategory::RateLimited` (429) instead.
        let status = self.category.default_status();
        (status, Json(self)).into_response()
    }
}
