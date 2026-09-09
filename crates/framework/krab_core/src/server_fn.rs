//! # Server Functions (`#[server]`)
//!
//! Server functions allow you to write async functions that run on the server
//! and can be called transparently from the client (WASM) via RPC.
//!
//! ## Usage
//!
//! ```rust
//! use krab_core::server_fn::ServerFnError;
//! use krab_macros::server;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! pub struct User {
//!     pub id: String,
//!     pub name: String,
//! }
//!
//! #[server]
//! pub async fn get_user(id: String) -> Result<User, ServerFnError> {
//!     find_user(&id).await.map_err(|e| ServerFnError::new(e.to_string()))
//! }
//!
//! # async fn find_user(id: &str) -> Result<User, std::io::Error> {
//! #     Ok(User { id: id.to_string(), name: "Ada".to_string() })
//! # }
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

    /// Create a validation error from a deserialization failure, with any
    /// caller-supplied values removed from the message.
    ///
    /// `#[server]` handlers use this rather than formatting the deserializer's
    /// message directly. Serde's diagnostics mix two kinds of information:
    ///
    /// - **Schema facts**, which the caller needs and which are safe to return:
    ///   the field name in a "missing field" error, the permitted set in an
    ///   "expected one of" list, the expected type, the line and column.
    /// - **Submitted values**, which must not come back: the quoted string in an
    ///   "invalid type: string" error, the backticked token in "unknown variant"
    ///   or "integer" errors — all of them quote what the caller sent. A rejected body routinely carries the very credential that made
    ///   it invalid, and error responses are among the most heavily logged
    ///   objects in a stack, so returning one writes it to every log that
    ///   touches the response.
    ///
    /// Returning nothing at all was the first attempt and it was too blunt: it
    /// also discarded the field name, which is what makes the error actionable.
    pub fn from_deserialization_error(function: &str, error: &serde_json::Error) -> Self {
        Self::validation(format!(
            "Validation failed for '{}': {}",
            function,
            redact_submitted_values(&error.to_string())
        ))
    }
}

/// Strip caller-supplied values out of a serde diagnostic, keeping schema facts.
///
/// Serde spells a submitted value in exactly two ways, and both are handled:
///
/// - **Double-quoted**, for strings: `invalid type: string "hunter2"`. The
///   quoted span is a `{:?}` rendering, so a `"` inside the value arrives as
///   `\"` — a scanner that closes on the first `"` would emit the tail of the
///   secret verbatim. Backslash escapes are honoured.
/// - **Backticked after a type word**, for everything else — the serde forms
///   "integer", "boolean", "floating point" and "character", each followed by
///   the value in backticks — and the enum/field forms "unknown variant" and
///   "unknown field", followed by the caller's token. Only the token after one
///   of those prefixes is submitted; "missing field" and "expected one of" are
///   followed by schema names, and keeping those is the point.
///
/// The first cut handled quoted strings and the two enum/field prefixes only,
/// which redacted a mistyped password but returned a mistyped PIN.
pub fn redact_submitted_values(message: &str) -> String {
    const REDACTED: &str = "<redacted>";
    // Every serde `Unexpected` variant that carries a value renders as the
    // type word followed by the value in backticks; the field/variant errors
    // do the same with a caller-supplied name. Anything else backticked in a
    // serde message is schema.
    const VALUE_PREFIXES: [&str; 6] = [
        "unknown variant `",
        "unknown field `",
        "integer `",
        "boolean `",
        "floating point `",
        "character `",
    ];

    // Pass one: backticked values, every occurrence.
    let mut pass_one = String::with_capacity(message.len());
    let mut rest = message;
    loop {
        let next = VALUE_PREFIXES
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|at| (at, *prefix)))
            .min_by_key(|(at, _)| *at);
        let Some((at, prefix)) = next else { break };
        let after = at + prefix.len();
        let Some(len) = rest[after..].find('`') else {
            break;
        };
        pass_one.push_str(&rest[..after]);
        pass_one.push_str(REDACTED);
        rest = &rest[after + len..];
    }
    pass_one.push_str(rest);

    // Pass two: quoted values, honouring backslash escapes inside them.
    let mut out = String::with_capacity(pass_one.len());
    let mut in_quotes = false;
    let mut chars = pass_one.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if in_quotes => {
                // An escaped character is part of the value; drop it and the
                // escape, and in particular do not let `\"` close the span.
                chars.next();
            }
            '"' if in_quotes => {
                out.push_str(REDACTED);
                out.push('"');
                in_quotes = false;
            }
            '"' => {
                out.push('"');
                in_quotes = true;
            }
            _ if in_quotes => {}
            _ => out.push(ch),
        }
    }
    if in_quotes {
        // Unterminated quote: the remainder was a value, so it stays dropped.
        out.push_str(REDACTED);
    }

    out
}

impl ServerFnError {
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

/// Longest non-JSON error body copied verbatim into a decoded error message.
/// Anything past this is upstream noise (an HTML error page, a proxy banner),
/// not signal worth carrying around.
const MAX_FALLBACK_ERROR_BODY_LEN: usize = 2_048;

/// Decode a server-function error response body into the canonical error type.
pub fn decode_server_fn_error_body(body: &str, fallback_status_code: u16) -> ServerFnError {
    if let Ok(envelope) = serde_json::from_str::<ServerFnErrorEnvelope>(body) {
        return envelope.into();
    }
    if let Ok(err) = serde_json::from_str::<ServerFnError>(body) {
        return err;
    }
    let truncated = if body.len() > MAX_FALLBACK_ERROR_BODY_LEN {
        let mut end = MAX_FALLBACK_ERROR_BODY_LEN;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        &body[..end]
    } else {
        body
    };
    ServerFnError::from_status(fallback_status_code, truncated)
}

impl From<serde_json::Error> for ServerFnError {
    fn from(err: serde_json::Error) -> Self {
        Self::bad_request(format!("serialization error: {}", err))
    }
}

impl From<anyhow::Error> for ServerFnError {
    /// Converts to a generic 500 without carrying the error text.
    ///
    /// The chain behind an `anyhow::Error` routinely contains connection
    /// strings, SQL fragments, or filesystem paths, and this envelope is
    /// serialized straight to the browser. The full chain goes to the server
    /// log instead; callers that want a client-visible message construct one
    /// deliberately via [`ServerFnError::new`].
    fn from(err: anyhow::Error) -> Self {
        tracing::error!(error = ?err, "server_fn_internal_error");
        Self::new("internal server error")
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
///
/// Two variants, selected by feature. The `rest` form carries the dispatch
/// handler; the client form is metadata only, because a browser build has no
/// axum `Response` to hand back. Previously only the field was gated while the
/// struct itself was not, so with `rest` off both definitions landed in the same
/// namespace — invisible while the whole module was gated behind `rest`, and an
/// immediate `E0428` once it was not.
#[cfg(feature = "rest")]
pub struct ServerFnRegistration {
    /// Function name (snake_case).
    pub name: &'static str,
    /// URL path for this function (e.g., `/api/rpc/get_user`).
    pub url: &'static str,
    /// Handler that accepts JSON args and returns an Axum response (can be streaming).
    pub handler: fn(serde_json::Value) -> BoxFuture<axum::response::Response>,
}

/// Client-side registration metadata. See the `rest` variant above.
#[cfg(not(feature = "rest"))]
pub struct ServerFnRegistration {
    /// Function name (snake_case).
    pub name: &'static str,
    /// URL path for this function (e.g., `/api/rpc/get_user`).
    pub url: &'static str,
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ServerFnRegistration>();
};

/// Compile-time metadata for a server function.
///
/// `#[server]` generates a hidden marker type named after the annotated
/// function and implements this trait for it. The marker lives only in the type
/// namespace (it is declared as `struct name {}`), so it coexists with the
/// function of the same name in the value namespace.
///
/// This is what lets [`collect_server_fns!`](crate::collect_server_fns) build a
/// registration table from bare function names without concatenating
/// identifiers — something `macro_rules!` cannot do on its own.
#[cfg(feature = "rest")]
pub trait ServerFn {
    /// The function name, as written in the source.
    const NAME: &'static str;
    /// The URL the generated handler is mounted at.
    const URL: &'static str;
    /// Decode JSON arguments, invoke the function, and encode the response.
    fn dispatch(args: serde_json::Value) -> BoxFuture<axum::response::Response>;
}

/// Build an Axum router from a list of server function registrations.
///
/// Each registration is mounted at its declared URL as a POST endpoint.
///
/// # Example
///
/// ```rust
/// use krab_core::collect_server_fns;
/// use krab_core::server_fn::{server_fn_router, ServerFn, ServerFnError, ServerFnRegistration};
/// use krab_macros::server;
///
/// #[server]
/// pub async fn get_user(id: String) -> Result<String, ServerFnError> {
///     Ok(format!("user:{id}"))
/// }
///
/// static SERVER_FNS: &[ServerFnRegistration] = &collect_server_fns![get_user];
///
/// let rpc_routes = server_fn_router(SERVER_FNS);
///
/// let app: axum::Router = axum::Router::new()
///     .merge(rpc_routes)
///     .route("/health", axum::routing::get(|| async { "ok" }));
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

    let opts = web_sys::RequestInit::new();
    opts.set_method("POST");
    opts.set_body(&wasm_bindgen::JsValue::from_str(&body));

    let request = web_sys::Request::new_with_str_and_init(url, &opts)
        .map_err(|_| ServerFnError::new("failed to create request"))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|_| ServerFnError::new("failed to set content-type"))?;

    // Propagate a CSRF token if the server issues them. The CSRF cookie is
    // `HttpOnly` by design, so it can never be read from `document.cookie` —
    // the token endpoint returns the token in its JSON body and sets the
    // matching cookie on that same response, which the browser attaches to
    // this request automatically. Header and cookie names are the shared
    // constants in [`crate::csrf`], so client and middleware cannot drift.
    if let Some(token) = fetch_csrf_token(&window).await {
        let _ = request.headers().set(crate::csrf::CSRF_HEADER_NAME, &token);
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

/// Fetch a CSRF token from [`crate::csrf::CSRF_TOKEN_ENDPOINT_PATH`].
///
/// Returns `None` when the endpoint is not mounted (404), unreachable, or the
/// body does not carry [`crate::csrf::CSRF_TOKEN_JSON_FIELD`] — in which case
/// the call proceeds without a CSRF header, exactly as before for deployments
/// that do not enable CSRF protection.
#[cfg(target_arch = "wasm32")]
async fn fetch_csrf_token(window: &web_sys::Window) -> Option<String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let opts = web_sys::RequestInit::new();
    opts.set_method("GET");

    let request =
        web_sys::Request::new_with_str_and_init(crate::csrf::CSRF_TOKEN_ENDPOINT_PATH, &opts)
            .ok()?;

    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .ok()?;
    let resp: web_sys::Response = resp_value.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }

    let text = JsFuture::from(resp.text().ok()?).await.ok()?;
    let text = text.as_string()?;
    let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
    parsed
        .get(crate::csrf::CSRF_TOKEN_JSON_FIELD)?
        .as_str()
        .map(ToString::to_string)
}

/// Default timeout for native server-function calls, overridable via
/// `KRAB_SERVER_FN_TIMEOUT_MS`.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_SERVER_FN_TIMEOUT_MS: u64 = 30_000;

/// Shared HTTP client for native server-function calls.
///
/// One client per process: reqwest has no default timeout, so a hung upstream
/// would otherwise block the calling task forever, and a per-call client
/// defeats connection pooling. The timeout is read once, at first use.
#[cfg(not(target_arch = "wasm32"))]
fn native_rpc_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        let timeout_ms = std::env::var("KRAB_SERVER_FN_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .unwrap_or(DEFAULT_SERVER_FN_TIMEOUT_MS);
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .build()
            .unwrap_or_default()
    })
}

/// Native client-side call path for non-WASM targets.
#[cfg(not(target_arch = "wasm32"))]
pub async fn call_server_fn<A: Serialize, T: serde::de::DeserializeOwned>(
    url: &str,
    args: &A,
) -> Result<T, ServerFnError> {
    let response = native_rpc_client()
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

/// Collect server function registrations into an array.
///
/// Takes the bare names of `#[server]`-annotated functions and resolves each
/// one's name, URL, and dispatch handler through the
/// [`ServerFn`](crate::server_fn::ServerFn) marker type that `#[server]`
/// generates. Requires the `rest` feature.
///
/// # Example
///
/// ```rust
/// use krab_core::collect_server_fns;
/// use krab_core::server_fn::{ServerFn, ServerFnError};
/// use krab_macros::server;
///
/// #[server]
/// pub async fn get_user(id: String) -> Result<String, ServerFnError> {
///     Ok(format!("user:{id}"))
/// }
///
/// #[server]
/// pub async fn list_items() -> Result<Vec<String>, ServerFnError> {
///     Ok(vec!["a".to_string()])
/// }
///
/// static SERVER_FNS: &[krab_core::server_fn::ServerFnRegistration] =
///     &collect_server_fns![get_user, list_items];
///
/// assert_eq!(SERVER_FNS.len(), 2);
/// assert_eq!(SERVER_FNS[0].name, "get_user");
/// assert_eq!(SERVER_FNS[0].url, "/api/rpc/get_user");
/// ```
#[macro_export]
macro_rules! collect_server_fns {
    ($($fn_name:ident),* $(,)?) => {
        [
            $($crate::server_fn::ServerFnRegistration {
                name: <$fn_name as $crate::server_fn::ServerFn>::NAME,
                url: <$fn_name as $crate::server_fn::ServerFn>::URL,
                handler: <$fn_name as $crate::server_fn::ServerFn>::dispatch,
            }),*
        ]
    };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_keeps_the_field_name_a_caller_needs() {
        // The whole reason this is not a blanket "return nothing": a missing
        // field is named by the schema, not by the caller, and without it the
        // error tells the caller only that something was wrong somewhere.
        let message = redact_submitted_values("missing field `excited`");
        assert_eq!(message, "missing field `excited`");
    }

    #[test]
    fn redaction_removes_a_submitted_value_but_keeps_the_expected_type() {
        let message = redact_submitted_values("invalid type: string \"hunter2\", expected u64");
        assert!(!message.contains("hunter2"), "value leaked: {message}");
        assert!(
            message.contains("expected u64"),
            "diagnostic lost: {message}"
        );
    }

    #[test]
    fn redaction_removes_an_unknown_variant_but_keeps_the_permitted_set() {
        let message =
            redact_submitted_values("unknown variant `hunter2`, expected one of `read`, `write`");
        assert!(!message.contains("hunter2"), "value leaked: {message}");
        assert!(message.contains("`read`"), "permitted set lost: {message}");
        assert!(message.contains("`write`"), "permitted set lost: {message}");
    }

    #[test]
    fn redaction_removes_non_string_values_too() {
        // serde renders a mistyped number, bool, float or char in backticks
        // after a type word, not in quotes. A `pin: String` field sent as
        // `123456` produced `invalid type: integer `123456`, expected a string`
        // in the first cut — the PIN, returned and logged.
        for (message, secret) in [
            (
                "invalid type: integer `123456`, expected a string",
                "123456",
            ),
            ("invalid type: boolean `true`, expected a string", "true"),
            ("invalid type: floating point `1.5`, expected u64", "1.5"),
            ("invalid value: character `Z`, expected a digit", "Z"),
        ] {
            let redacted = redact_submitted_values(message);
            assert!(!redacted.contains(secret), "value leaked: {redacted}");
            assert!(redacted.contains("expected"), "diagnostic lost: {redacted}");
        }
    }

    #[test]
    fn redaction_honours_escaped_quotes_inside_a_string_value() {
        // `{:?}` renders an embedded `"` as `\\"`. A scanner that closes on it
        // emits everything after it — here, the whole secret.
        let redacted =
            redact_submitted_values(r#"invalid type: string "x\"hunter2", expected u64"#);
        assert!(
            !redacted.contains("hunter2"),
            "value leaked past an escaped quote: {redacted}"
        );
        assert!(
            redacted.contains("expected u64"),
            "diagnostic lost: {redacted}"
        );
    }

    #[test]
    fn redaction_handles_a_message_with_several_values() {
        let redacted = redact_submitted_values(
            r#"invalid value: integer `42`, expected one of `read`, `write`; got string "hunter2""#,
        );
        assert!(!redacted.contains("42"), "integer leaked: {redacted}");
        assert!(!redacted.contains("hunter2"), "string leaked: {redacted}");
        assert!(
            redacted.contains("`read`") && redacted.contains("`write`"),
            "schema lost: {redacted}"
        );
    }

    #[test]
    fn redaction_survives_an_unterminated_quote() {
        // Malformed input must not produce a message that leaks the tail it
        // failed to close.
        let message = redact_submitted_values("invalid type: string \"secret");
        assert!(!message.contains("secret"), "value leaked: {message}");
    }

    #[test]
    fn deserialization_errors_name_the_function_and_drop_the_value() {
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct Args {
            count: u64,
        }

        let error =
            serde_json::from_value::<Args>(serde_json::json!({ "count": "hunter2" })).unwrap_err();
        let err = ServerFnError::from_deserialization_error("greet", &error);

        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("greet"), "function lost: {err}");
        assert!(!err.message.contains("hunter2"), "value leaked: {err}");
    }

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
    fn server_fn_error_from_anyhow_does_not_leak_internal_detail() {
        let anyhow_err = anyhow::anyhow!("postgres://user:s3cret@db/prod: connection refused");
        let err: ServerFnError = anyhow_err.into();
        assert_eq!(err.status_code, 500);
        assert_eq!(err.message, "internal server error");
        assert!(!err.message.contains("s3cret"));
    }

    #[test]
    fn decode_server_fn_error_body_truncates_oversized_fallback_bodies() {
        let body = "x".repeat(MAX_FALLBACK_ERROR_BODY_LEN * 4);
        let err = decode_server_fn_error_body(&body, 502);
        assert_eq!(err.status_code, 502);
        assert_eq!(err.message.len(), MAX_FALLBACK_ERROR_BODY_LEN);
    }

    #[test]
    fn server_fn_error_from_serde() {
        let serde_err = serde_json::from_str::<String>("not valid json").unwrap_err();
        let err: ServerFnError = serde_err.into();
        assert_eq!(err.status_code, 400);
        assert!(err.message.contains("serialization error"));
    }
}
