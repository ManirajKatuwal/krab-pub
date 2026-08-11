use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::State;
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::SocketAddr;
use tracing::warn;

use crate::http::{bool_env, constant_time_eq, HasRuntimeState};

pub fn extract_client_ip(req: &Request<Body>, trust_proxy_headers: bool) -> String {
    if trust_proxy_headers {
        if let Some(value) = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok())
        {
            if let Some(first) = value.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }

        if let Some(value) = req.headers().get("x-real-ip").and_then(|h| h.to_str().ok()) {
            let ip = value.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }

    if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return connect_info.0.ip().to_string();
    }

    "unknown".to_string()
}

pub fn csrf_protection_enabled() -> bool {
    bool_env("KRAB_CSRF_ENABLED", false) || bool_env("KRAB_AUTH_COOKIE_SESSION_ENABLED", false)
}

pub fn is_unsafe_http_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

pub fn csrf_cookie_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get("cookie")?.to_str().ok()?;
    raw.split(';').map(str::trim).find_map(|pair| {
        pair.strip_prefix(crate::csrf::CSRF_COOKIE_NAME)
            .and_then(|rest| rest.strip_prefix('='))
            .map(ToString::to_string)
    })
}

pub fn csrf_header_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(crate::csrf::CSRF_HEADER_NAME)
        .and_then(|h| h.to_str().ok())
        .map(ToString::to_string)
}

/// Issue a CSRF token: the same value is set as an `HttpOnly` cookie and
/// returned in the JSON body under [`crate::csrf::CSRF_TOKEN_JSON_FIELD`],
/// because an `HttpOnly` cookie is unreadable from script by design — the
/// body is the only way a browser client can learn the token. Mount at
/// [`crate::csrf::CSRF_TOKEN_ENDPOINT_PATH`] for the wasm client to find it.
pub async fn csrf_token_endpoint() -> Response {
    let token = uuid::Uuid::new_v4().to_string();
    let cookie_value = format!(
        "{}={}; SameSite=Strict; HttpOnly; Secure; Path=/",
        crate::csrf::CSRF_COOKIE_NAME,
        token
    );
    let mut body = serde_json::Map::new();
    body.insert(
        crate::csrf::CSRF_TOKEN_JSON_FIELD.to_string(),
        serde_json::Value::String(token),
    );
    let mut response = Json(serde_json::Value::Object(body)).into_response();
    if let Ok(val) = HeaderValue::from_str(&cookie_value) {
        response
            .headers_mut()
            .insert(axum::http::header::SET_COOKIE, val);
    }
    response
}

pub async fn csrf_protection_middleware<S>(
    State(_state): State<S>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode>
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    if !csrf_protection_enabled() {
        return Ok(next.run(req).await);
    }

    if !is_unsafe_http_method(req.method()) {
        return Ok(next.run(req).await);
    }

    if req.headers().get("cookie").is_none() {
        return Ok(next.run(req).await);
    }

    let cookie_token = csrf_cookie_token(req.headers()).unwrap_or_default();
    let header_token = csrf_header_token(req.headers()).unwrap_or_default();

    if cookie_token.is_empty()
        || header_token.is_empty()
        || !constant_time_eq(cookie_token.as_bytes(), header_token.as_bytes())
    {
        warn!("csrf_token_validation_failed");
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(req).await)
}
