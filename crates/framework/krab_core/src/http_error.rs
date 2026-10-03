//! The JSON error envelope every Krab HTTP surface answers with.
//!
//! An [`ApiError`] serialises as `{"category", "code", "message", ...}` and,
//! as an axum response, takes its status from its [`ErrorCategory`] alone.

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
    /// The request is malformed or fails validation (HTTP 400).
    Validation,
    /// The request carries no acceptable credentials (HTTP 401).
    Unauthenticated,
    /// The caller is authenticated but not permitted to do this (HTTP 403).
    Authz,
    /// The addressed resource does not exist (HTTP 404).
    NotFound,
    /// The request conflicts with the resource's current state (HTTP 409).
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
    /// An unexpected server-side failure (HTTP 500).
    Internal,
}

impl ErrorCategory {
    /// The HTTP status an [`ApiError`] of this category is answered with.
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

/// A structured API error, serialised as the JSON response body.
///
/// Implements axum's `IntoResponse`: the status is
/// [`ErrorCategory::default_status`] of `category`, and the body is this
/// struct as JSON. Optional fields that are `None` are omitted from the JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    /// The failure class; decides the HTTP status. Serialised in snake_case.
    pub category: ErrorCategory,
    /// Machine-readable error code, conventionally SCREAMING_SNAKE_CASE (for
    /// example `PROTOCOL_NOT_SUPPORTED`). Clients branch on this.
    pub code: String,
    /// Human-readable description, for logs and developers rather than for
    /// parsing.
    pub message: String,
    /// Optional structured context, set with [`ApiError::with_details`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Optional request id for correlation. [`ApiError::new`] leaves it
    /// `None`; nothing in `krab_core` fills it in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Optional trace id for correlation. [`ApiError::new`] leaves it
    /// `None`; nothing in `krab_core` fills it in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl ApiError {
    /// An error with the given category, code and message, and no details or
    /// correlation ids.
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

    /// Attaches structured `details`, replacing any already set.
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
