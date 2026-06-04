use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::http::{compute_cors_origin, HasRuntimeState};

pub async fn security_headers_middleware(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("geolocation=(), camera=(), microphone=()"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-embedder-policy"),
        HeaderValue::from_static("require-corp"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("x-permitted-cross-domain-policies"),
        HeaderValue::from_static("none"),
    );

    response
}

pub fn cors_allow_methods_value() -> &'static str {
    "GET,POST,OPTIONS"
}

pub fn cors_allow_headers_value() -> &'static str {
    "authorization,content-type,x-request-id,x-trace-id"
}

fn cors_preflight_response(origin: &str) -> Option<Response> {
    let origin_header = HeaderValue::from_str(origin).ok()?;
    let methods_header = HeaderValue::from_str(cors_allow_methods_value()).ok()?;
    let headers_header = HeaderValue::from_str(cors_allow_headers_value()).ok()?;

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NO_CONTENT;
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin_header);
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_METHODS, methods_header);
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_HEADERS, headers_header);
    Some(resp)
}

fn append_cors_headers(resp: &mut Response, origin: &str) -> bool {
    let origin_header = match HeaderValue::from_str(origin) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let methods_header = match HeaderValue::from_str(cors_allow_methods_value()) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let headers_header = match HeaderValue::from_str(cors_allow_headers_value()) {
        Ok(value) => value,
        Err(_) => return false,
    };

    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin_header);
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_METHODS, methods_header);
    resp.headers_mut()
        .insert(ACCESS_CONTROL_ALLOW_HEADERS, headers_header);
    true
}

pub async fn cors_middleware<S>(State(state): State<S>, req: Request<Body>, next: Next) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let request_origin = req
        .headers()
        .get("origin")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let runtime = state.runtime_state();
    let allowed_origin: Option<String> = compute_cors_origin(
        &request_origin,
        &runtime.cors_origins,
        runtime.cors_allow_any_origin,
    )
    .map(|s| s.to_string());

    if req.method() == Method::OPTIONS {
        match allowed_origin {
            Some(origin) => {
                if let Some(resp) = cors_preflight_response(&origin) {
                    return resp;
                }

                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                return resp;
            }
            None => {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::FORBIDDEN;
                return resp;
            }
        }
    }

    let mut resp = next.run(req).await;
    if let Some(origin) = allowed_origin {
        if !append_cors_headers(&mut resp, &origin) {
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        }
    }
    resp
}
