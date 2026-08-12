use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    STRICT_TRANSPORT_SECURITY, VARY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
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

/// Mark the response as varying by request `Origin` so shared caches key on
/// it — required whenever the `Access-Control-Allow-Origin` decision depends
/// on the request origin.
fn append_vary_origin(resp: &mut Response) {
    resp.headers_mut()
        .append(VARY, HeaderValue::from_static("origin"));
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
    append_vary_origin(&mut resp);
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
    append_vary_origin(resp);
    true
}

pub async fn cors_middleware<S>(State(state): State<S>, req: Request<Body>, next: Next) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let has_origin_header = req.headers().contains_key("origin");
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

    // An OPTIONS request without an Origin header is not a CORS preflight —
    // it falls through to the router like any other request. Only OPTIONS
    // carrying an Origin is treated as preflight and answered here.
    if req.method() == Method::OPTIONS && has_origin_header {
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
                append_vary_origin(&mut resp);
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::{ACCESS_CONTROL_ALLOW_ORIGIN, VARY};
    use axum::http::{Method, Request, StatusCode};
    use axum::middleware;
    use axum::Router;
    use tower::ServiceExt;

    use super::cors_middleware;
    use crate::http::{HasRuntimeState, RuntimeState};

    #[derive(Clone)]
    struct TestState {
        runtime: RuntimeState,
    }

    impl HasRuntimeState for TestState {
        fn runtime_state(&self) -> &RuntimeState {
            &self.runtime
        }
    }

    /// Router with only the CORS middleware applied, configured with a fixed
    /// origin allowlist so the tests do not depend on process env.
    fn cors_app(allowed: &[&str]) -> Router {
        let mut runtime = RuntimeState::new();
        runtime.cors_origins = allowed.iter().map(|s| s.to_string()).collect();
        runtime.cors_allow_any_origin = false;
        let state = TestState { runtime };

        Router::new()
            .route(
                "/",
                axum::routing::get(|| async { "get-ok" }).options(|| async { "options-ok" }),
            )
            .layer(middleware::from_fn_with_state(
                state,
                cors_middleware::<TestState>,
            ))
    }

    fn vary_values(resp: &axum::response::Response) -> Vec<String> {
        resp.headers()
            .get_all(VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase())
            .collect()
    }

    #[tokio::test]
    async fn options_without_origin_is_not_cors_and_reaches_the_router() {
        let app = cors_app(&["https://app.example.com"]);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        // The route's own OPTIONS handler answered — not the middleware's 403.
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    }

    #[tokio::test]
    async fn preflight_with_allowed_origin_carries_acao_and_vary_origin() {
        let app = cors_app(&["https://app.example.com"]);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/")
                    .header("origin", "https://app.example.com")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://app.example.com")
        );
        assert!(vary_values(&resp).contains(&"origin".to_string()));
    }

    #[tokio::test]
    async fn main_path_with_allowed_origin_carries_acao_and_vary_origin() {
        let app = cors_app(&["https://app.example.com"]);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/")
                    .header("origin", "https://app.example.com")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("https://app.example.com")
        );
        assert!(vary_values(&resp).contains(&"origin".to_string()));
    }

    #[tokio::test]
    async fn preflight_with_disallowed_origin_is_403_with_vary_origin() {
        let app = cors_app(&["https://app.example.com"]);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/")
                    .header("origin", "https://evil.example.com")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
        assert!(vary_values(&resp).contains(&"origin".to_string()));
    }
}
