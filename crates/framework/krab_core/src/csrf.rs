//! CSRF wire-contract names shared by the server-side protection middleware
//! and the wasm client half of `#[server]`.
//!
//! Both halves must agree on four names: the cookie the server sets, the
//! header the middleware validates, the endpoint that issues tokens, and the
//! JSON field carrying the token in that endpoint's response body. They used
//! to be duplicated string literals — the client read a cookie named
//! `csrf_token` while the server set `krab_csrf_token`, so the shipped client
//! could never satisfy the middleware. Single-sourcing them here makes that
//! drift impossible; `http_security` and `server_fn` both reference these
//! constants, and a native test pins the endpoint's output to them.
//!
//! This module is compiled on every target and feature set on purpose: the
//! server half (`rest`) and the browser half (`web` on `wasm32`) are never
//! compiled together, so the shared names cannot live behind either gate.

/// Cookie set by [`crate::http_security::csrf_token_endpoint`] and validated
/// by `csrf_protection_middleware`. `HttpOnly`, so it is deliberately
/// unreadable from `document.cookie`.
pub const CSRF_COOKIE_NAME: &str = "krab_csrf_token";

/// Request header carrying the token copy that must match the cookie.
pub const CSRF_HEADER_NAME: &str = "x-csrf-token";

/// Conventional mount path for `csrf_token_endpoint`. The wasm client fetches
/// this path to obtain a token before issuing a state-changing request; mount
/// the endpoint here (and list it as an open/public path) when CSRF
/// protection is enabled.
pub const CSRF_TOKEN_ENDPOINT_PATH: &str = "/api/csrf-token";

/// JSON field in the token endpoint's response body that carries the token.
pub const CSRF_TOKEN_JSON_FIELD: &str = "csrf_token";
