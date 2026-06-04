//! # Server Functions (`#[server]`)
//!
//! Server functions allow you to write async functions that run on the server
//! and can be called transparently from the client (WASM) via RPC.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use krab_macros::server;
//! use krab_core::server_fn::ServerFnError;
//!
//! #[server]
//! pub async fn get_user(id: String) -> Result<User, ServerFnError> {
//!     db::find_user(&id).await.map_err(|e| ServerFnError::new(e.to_string()))
//! }
//! ```
//!
//! On the server, this keeps the function as-is and generates an Axum handler.
//! On the client (WASM), the body is replaced with a `fetch` call to `/api/rpc/get_user`.
//!
//! Once a generated handler is mounted, the server function is a public HTTP POST endpoint.
//! Validate all inputs and enforce auth inside the function or in the mounted router stack.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

// ── Error Type ──────────────────────────────────────────────────────────────

/// Stable error category carried over the server-function wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFnErrorCode {
    BadRequest,
    Validation,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Internal,
}

fn default_error_code() -> ServerFnErrorCode {
    ServerFnErrorCode::Internal
}

/// Error type returned by server functions.
///
/// Implements `Serialize`/`Deserialize` for wire transport, and converts
/// into an Axum response when used on the server side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFnError {
    /// Human-readable error message.
    pub message: String,
    /// HTTP status code for the error response.
    #[serde(default = "default_status")]
    pub status_code: u16,
    /// Stable machine-readable error category.
    #[serde(default = "default_error_code")]
    pub code: ServerFnErrorCode,
}

/// Wire response envelope emitted by server-function HTTP handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFnErrorEnvelope {
    /// Legacy-compatible error string.
    pub error: String,
    /// Canonical error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// HTTP status code for the error response.
    #[serde(default = "default_status")]
    pub status_code: u16,
    /// Stable machine-readable error category.
    #[serde(default = "default_error_code")]
    pub code: ServerFnErrorCode,
}

impl From<ServerFnError> for ServerFnErrorEnvelope {
    fn from(err: ServerFnError) -> Self {
        Self {
            error: err.message.clone(),
            message: Some(err.message),
            status_code: err.status_code,
            code: err.code,
        }
    }
}

impl From<ServerFnErrorEnvelope> for ServerFnError {
    fn from(envelope: ServerFnErrorEnvelope) -> Self {
        Self {
            message: envelope.message.unwrap_or(envelope.error),
            status_code: envelope.status_code,
            code: envelope.code,
        }
    }
}

fn default_status() -> u16 {
    500
}

impl std::fmt::Display for ServerFnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServerFnError({}): {}", self.status_code, self.message)
    }
}

impl std::error::Error for ServerFnError {}

impl ServerFnError {
    fn with_code(message: impl Into<String>, status_code: u16, code: ServerFnErrorCode) -> Self {
        Self {
            message: message.into(),
            status_code,
            code,
        }
    }

    /// Create a new server error with HTTP 500.
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_code(message, 500, ServerFnErrorCode::Internal)
    }

    /// Create a bad request error (HTTP 400).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::with_code(message, 400, ServerFnErrorCode::BadRequest)
    }

    /// Create a validation error (HTTP 400).
    pub fn validation(message: impl Into<String>) -> Self {
        Self::with_code(message, 400, ServerFnErrorCode::Validation)
    }

    /// Create an unauthorized error (HTTP 401).
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::with_code(message, 401, ServerFnErrorCode::Unauthorized)
    }

    /// Create a forbidden error (HTTP 403).
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::with_code(message, 403, ServerFnErrorCode::Forbidden)
    }

    /// Create a not found error (HTTP 404).
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::with_code(message, 404, ServerFnErrorCode::NotFound)
    }

    /// Create a conflict error (HTTP 409).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::with_code(message, 409, ServerFnErrorCode::Conflict)
    }

    /// Create an error from an HTTP status code, preserving the closest known category.
    pub fn from_status(status_code: u16, message: impl Into<String>) -> Self {
        let code = match status_code {
            400 => ServerFnErrorCode::BadRequest,
            401 => ServerFnErrorCode::Unauthorized,
            403 => ServerFnErrorCode::Forbidden,
            404 => ServerFnErrorCode::NotFound,
            409 => ServerFnErrorCode::Conflict,
            _ => ServerFnErrorCode::Internal,
        };
        Self::with_code(message, status_code, code)
    }
}

/// Return a validation error when a server-function precondition is false.
pub fn validate_server_fn(
    condition: bool,
    message: impl Into<String>,
) -> Result<(), ServerFnError> {
    if condition {
        Ok(())
    } else {
        Err(ServerFnError::validation(message))
    }
}

/// Require an authenticated caller before running a server-function mutation.
pub fn require_server_fn_auth(
    authenticated: bool,
    message: impl Into<String>,
) -> Result<(), ServerFnError> {
    if authenticated {
        Ok(())
    } else {
        Err(ServerFnError::unauthorized(message))
    }
}

/// Require a caller scope before running a protected server function.
pub fn require_server_fn_scope<I, S>(scopes: I, required: &str) -> Result<(), ServerFnError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if scopes.into_iter().any(|scope| scope.as_ref() == required) {
        Ok(())
    } else {
        Err(ServerFnError::forbidden(format!(
            "missing required scope '{required}'"
        )))
    }
}

/// Decode a server-function error response body into the canonical error type.
pub fn decode_server_fn_error_body(body: &str, fallback_status_code: u16) -> ServerFnError {
    if let Ok(envelope) = serde_json::from_str::<ServerFnErrorEnvelope>(body) {
        return envelope.into();
    }
    if let Ok(err) = serde_json::from_str::<ServerFnError>(body) {
        return err;
    }
    ServerFnError::from_status(fallback_status_code, body)
}

impl From<serde_json::Error> for ServerFnError {
    fn from(err: serde_json::Error) -> Self {
        Self::bad_request(format!("serialization error: {}", err))
    }
}

impl From<anyhow::Error> for ServerFnError {
    fn from(err: anyhow::Error) -> Self {
        Self::new(err.to_string())
    }
}

// ── Axum Integration (server side) ──────────────────────────────────────────

#[cfg(feature = "rest")]
impl axum::response::IntoResponse for ServerFnError {
    fn into_response(self) -> axum::response::Response {
        let status = axum::http::StatusCode::from_u16(self.status_code)
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let body = ServerFnErrorEnvelope::from(self);
        (status, axum::Json(body)).into_response()
    }
}

// ── Registration ────────────────────────────────────────────────────────────

/// Type alias for server function handler.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// A registered server function entry point.
pub struct ServerFnRegistration {
    /// Function name (snake_case).
    pub name: &'static str,
    /// URL path for this function (e.g., `/api/rpc/get_user`).
    pub url: &'static str,
    /// Handler that accepts JSON args and returns an Axum response (can be streaming).
    #[cfg(feature = "rest")]
    pub handler: fn(serde_json::Value) -> BoxFuture<axum::response::Response>,
}

#[cfg(not(feature = "rest"))]
pub struct ServerFnRegistration {
    pub name: &'static str,
    pub url: &'static str,
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ServerFnRegistration>();
};

/// Build an Axum router from a list of server function registrations.
///
/// Each registration is mounted at its declared URL as a POST endpoint.
///
/// # Example
///
/// ```rust,ignore
/// use krab_core::server_fn::server_fn_router;
///
/// let rpc_routes = server_fn_router(&[
///     ServerFnRegistration {
///         name: "get_user",
///         url: "/api/rpc/get_user",
///         handler: __get_user_handler,
///     },
/// ]);
///
/// let app = Router::new()
///     .merge(rpc_routes)
///     .route("/health", get(health));
/// ```
#[cfg(feature = "rest")]
pub fn server_fn_router(registrations: &'static [ServerFnRegistration]) -> axum::Router {
    use axum::routing::post;

    let mut router = axum::Router::new();
    for reg in registrations {
        let handler = reg.handler;
        router = router.route(
            reg.url,
            post(
                move |axum::Json(args): axum::Json<serde_json::Value>| async move {
                    handler(args).await
                },
            ),
        );
    }
    router
}

/// Build a catch-all RPC dispatcher that routes `/api/rpc/:fn_name` to
/// matching registrations.
///
/// This is an alternative to `server_fn_router` for simpler wiring.
#[cfg(feature = "rest")]
pub fn server_fn_dispatch_router(registrations: &'static [ServerFnRegistration]) -> axum::Router {
    router_with_dispatch(registrations)
}

#[cfg(feature = "rest")]
fn router_with_dispatch(registrations: &'static [ServerFnRegistration]) -> axum::Router {
    use axum::extract::Path;
    use axum::response::IntoResponse;

    axum::Router::new().route(
        "/api/rpc/{fn_name}",
        axum::routing::post(
            move |Path(fn_name): Path<String>,
                  axum::Json(args): axum::Json<serde_json::Value>| async move {
                for reg in registrations {
                    if reg.name == fn_name {
                        return (reg.handler)(args).await;
                    }
                }
                ServerFnError::not_found(format!("server function '{}' not found", fn_name))
                    .into_response()
            },
        ),
    )
}

// ── Client-Side Call (WASM) ─────────────────────────────────────────────────

/// Call a server function from the client (WASM) via fetch.
///
/// This is used by the `#[server]` macro in the WASM client stub.
#[cfg(target_arch = "wasm32")]
pub async fn call_server_fn<A: Serialize, T: serde::de::DeserializeOwned>(
    url: &str,
    args: &A,
) -> Result<T, ServerFnError> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window().ok_or_else(|| ServerFnError::new("no window object"))?;
    let body = serde_json::to_string(args).map_err(|e| ServerFnError::new(e.to_string()))?;

    let mut opts = web_sys::RequestInit::new();
    opts.method("POST");
    opts.body(Some(&wasm_bindgen::JsValue::from_str(&body)));

    let request = web_sys::Request::new_with_str_and_init(url, &opts)
        .map_err(|_| ServerFnError::new("failed to create request"))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|_| ServerFnError::new("failed to set content-type"))?;

    // Propagate CSRF token if present
    if let Some(document) = window.document() {
        if let Ok(cookie) =
            js_sys::Reflect::get(&document, &wasm_bindgen::JsValue::from_str("cookie"))
        {
            let cookie_str = cookie.as_string().unwrap_or_default();
            for part in cookie_str.split(';') {
                let trimmed = part.trim();
                if let Some(token) = trimmed.strip_prefix("csrf_token=") {
                    let _ = request.headers().set("x-csrf-token", token);
                }
            }
        }
    }

    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| ServerFnError::new("fetch failed"))?;

    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|_| ServerFnError::new("response cast failed"))?;

    let text = JsFuture::from(
        resp.text()
            .map_err(|_| ServerFnError::new("failed to read response body"))?,
    )
    .await
    .map_err(|_| ServerFnError::new("failed to await response text"))?;

    let text_str = text
        .as_string()
        .ok_or_else(|| ServerFnError::new("response is not a string"))?;

    if resp.ok() {
        serde_json::from_str(&text_str).map_err(|e| ServerFnError::new(e.to_string()))
    } else {
        // Try to parse server error
        Err(decode_server_fn_error_body(&text_str, resp.status()))
    }
}

/// Native client-side call path for non-WASM targets.
#[cfg(not(target_arch = "wasm32"))]
pub async fn call_server_fn<A: Serialize, T: serde::de::DeserializeOwned>(
    url: &str,
    args: &A,
) -> Result<T, ServerFnError> {
    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .json(args)
        .send()
        .await
        .map_err(|err| ServerFnError::new(format!("request failed: {err}")))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| ServerFnError::new(format!("failed to read response body: {err}")))?;

    if status.is_success() {
        serde_json::from_str(&body)
            .map_err(|err| ServerFnError::new(format!("invalid success payload: {err}")))
    } else {
        Err(decode_server_fn_error_body(&body, status.as_u16()))
    }
}

// ── Convenience Macro ───────────────────────────────────────────────────────

/// Macro to collect server function registrations into a static slice.
///
/// # Example
///
/// ```rust,ignore
/// use krab_core::collect_server_fns;
///
/// static SERVER_FNS: &[krab_core::server_fn::ServerFnRegistration] =
///     &collect_server_fns![get_user, list_items, create_item];
/// ```
#[macro_export]
macro_rules! collect_server_fns {
    ($($fn_name:ident),* $(,)?) => {
        [
            $($crate::server_fn::ServerFnRegistration {
                name: stringify!($fn_name),
                url: concat!("/api/rpc/", stringify!($fn_name)),
                handler: paste::paste! { [<__ $fn_name _handler>] },
            }),*
        ]
    };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_fn_error_display() {
        let err = ServerFnError::new("something went wrong");
        assert_eq!(err.to_string(), "ServerFnError(500): something went wrong");
        assert_eq!(err.status_code, 500);
    }

    #[test]
    fn server_fn_error_constructors() {
        assert_eq!(ServerFnError::bad_request("bad").status_code, 400);
        assert_eq!(
            ServerFnError::validation("bad input").code,
            ServerFnErrorCode::Validation
        );
        assert_eq!(ServerFnError::unauthorized("no").status_code, 401);
        assert_eq!(ServerFnError::forbidden("denied").status_code, 403);
        assert_eq!(ServerFnError::not_found("gone").status_code, 404);
        assert_eq!(ServerFnError::conflict("dup").status_code, 409);
    }

    #[test]
    fn server_fn_error_serialization() {
        let err = ServerFnError::bad_request("invalid input");
        let json = serde_json::to_string(&err).unwrap();
        let parsed: ServerFnError = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message, "invalid input");
        assert_eq!(parsed.status_code, 400);
        assert_eq!(parsed.code, ServerFnErrorCode::BadRequest);
    }

    #[test]
    fn server_fn_error_deserializes_legacy_error_envelope() {
        let parsed =
            decode_server_fn_error_body(r#"{"error":"invalid payload","status_code":400}"#, 500);
        assert_eq!(parsed.message, "invalid payload");
        assert_eq!(parsed.status_code, 400);
        assert_eq!(parsed.code, ServerFnErrorCode::Internal);
    }

    #[test]
    fn server_fn_error_envelope_is_client_deserializable() {
        let envelope = ServerFnErrorEnvelope::from(ServerFnError::validation("missing field"));
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed = decode_server_fn_error_body(&json, 500);

        assert_eq!(parsed.message, "missing field");
        assert_eq!(parsed.status_code, 400);
        assert_eq!(parsed.code, ServerFnErrorCode::Validation);
    }

    #[test]
    fn validation_and_auth_helpers_return_typed_errors() {
        assert!(validate_server_fn(true, "ok").is_ok());
        let validation = validate_server_fn(false, "name is required").unwrap_err();
        assert_eq!(validation.status_code, 400);
        assert_eq!(validation.code, ServerFnErrorCode::Validation);

        let unauthorized = require_server_fn_auth(false, "login required").unwrap_err();
        assert_eq!(unauthorized.status_code, 401);
        assert_eq!(unauthorized.code, ServerFnErrorCode::Unauthorized);

        let forbidden = require_server_fn_scope(["users:read"], "users:write").unwrap_err();
        assert_eq!(forbidden.status_code, 403);
        assert_eq!(forbidden.code, ServerFnErrorCode::Forbidden);
    }

    #[test]
    fn server_fn_error_from_anyhow() {
        let anyhow_err = anyhow::anyhow!("something failed");
        let err: ServerFnError = anyhow_err.into();
        assert_eq!(err.status_code, 500);
        assert!(err.message.contains("something failed"));
    }

    #[test]
    fn server_fn_error_from_serde() {
        let serde_err = serde_json::from_str::<String>("not valid json").unwrap_err();
        let err: ServerFnError = serde_err.into();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("serialization error"));
    }
}
