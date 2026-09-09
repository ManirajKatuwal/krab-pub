use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of failure an [`ApiError`] represents. HTTP status derives from
/// this alone — see [`ErrorCategory::default_status`].
///
/// `#[non_exhaustive]`: adding a category is a routine, expected change (0.5.0
/// adds [`ErrorCategory::Unavailable`]), and without this every one of them
/// would break downstream `match` arms. Callers matching on a category need a
/// wildcard arm. This does not restrict *constructing* the existing variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCategory {
    Validation,
    /// The request carries no acceptable credentials (HTTP 401).
    Unauthenticated,
    Authz,
    NotFound,
    Conflict,
    /// The caller exceeded a rate limit (HTTP 429).
    RateLimited,
    /// The service cannot take the request right now (HTTP 503).
    ///
    /// Distinct from [`ErrorCategory::RateLimited`] on purpose: 429 says
    /// *this caller* asked for too much, 503 says *the service* is out of
    /// capacity. Load shedding is the latter, and load balancers and alert
    /// rules key off the difference.
    Unavailable,
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
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
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
