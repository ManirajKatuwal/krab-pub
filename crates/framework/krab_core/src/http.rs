use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Request};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;

use tracing::warn;

use crate::http_auth::{auth_middleware, service_auth_middleware};
pub use crate::http_auth::{
    authorize_with_jwt, authorize_with_static_bearer, enforce_claim_policy, has_admin_entitlement,
    is_internal_service_path, jwt_algorithm_allowed, jwt_leeway_secs, load_jwt_providers,
    load_rotation_keys, load_route_policies, require_kid, roles_from_claims, scopes_from_claims,
    select_key, tenant_from_claims, tenant_from_path, validate_provider_claims, AuthContext,
    JwtClaims, JwtProviderConfig, RoutePolicy,
};
pub use crate::http_error::{ApiError, ErrorCategory};
pub use crate::http_headers::{
    cors_allow_headers_value, cors_allow_methods_value, cors_middleware,
    security_headers_middleware,
};
pub use crate::http_observability::PropagationHeaders;
use crate::http_observability::{metrics_middleware, request_id_middleware, tracing_middleware};
use crate::http_protocol::protocol_resolution_middleware;
pub use crate::http_protocol::{
    extract_protocol_preference, route_family_protocol, runtime_switch_header_rejected_by_default,
};
pub use crate::http_runtime::{
    health, metrics, metrics_prometheus, readiness, readiness_with_dependencies, DependencyStatus,
    HasReadinessDependencies, HasRuntimeState, MetricsPayload, ReadinessPayload, RuntimeState,
    StatusPayload,
};
pub use crate::http_security::{
    csrf_cookie_token, csrf_header_token, csrf_protection_enabled, csrf_protection_middleware,
    csrf_token_endpoint, extract_client_ip, is_unsafe_http_method,
};

pub(crate) fn current_window_epoch(window_secs: u64) -> u64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe_window = window_secs.max(1);
    secs / safe_window
}

pub(crate) fn parse_csv_set(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

pub fn apply_common_http_layers<S>(router: Router<S>, state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    // Layer ordering note: axum/tower layers wrap bottom-up, so the LAST
    // `.layer(..)` in this chain is the OUTERMOST middleware and runs FIRST
    // for a request. `auth_middleware` must therefore be layered AFTER
    // `service_auth_middleware` here: `service_auth_middleware` reads the
    // `AuthContext` extension that `auth_middleware` inserts, so on the
    // request path auth must run first. It used to be the other way around,
    // which made every `/internal` request a 403 — the scope check ran before
    // any AuthContext could exist.
    router
        .layer(middleware::from_fn(security_headers_middleware))
        .layer(middleware::from_fn(api_version_header_middleware))
        .layer(CompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(1024 * 1024 * 2))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            service_auth_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            csrf_protection_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            global_rate_limit_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            cors_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            tracing_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_id_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            metrics_middleware::<S>,
        ))
        .layer(middleware::from_fn_with_state(
            state,
            protocol_resolution_middleware::<S>,
        ))
}

async fn api_version_header_middleware(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-krab-api-version"),
        HeaderValue::from_static("1"),
    );
    response
}

async fn global_rate_limit_middleware<S>(
    State(state): State<S>,
    req: Request<Body>,
    next: Next,
) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let (client_ip, capacity, refill_per_second, fail_open) = {
        let runtime = state.runtime_state();
        (
            extract_client_ip(&req, runtime.trust_proxy_headers),
            runtime.rate_limit_capacity,
            runtime.rate_limit_refill_per_sec,
            runtime.rate_limit_fail_open,
        )
    };

    let window_secs = ((capacity / refill_per_second.max(1.0)).ceil() as u64).clamp(1, 300);
    let window_epoch = current_window_epoch(window_secs);
    let key = format!("rate:ip:{}:{}", client_ip, window_epoch);

    let allowed = match state.runtime_state().store.incr(&key, 1).await {
        Ok(count) => {
            if count == 1 {
                let _ = state
                    .runtime_state()
                    .store
                    .expire(&key, Duration::from_secs(window_secs + 2))
                    .await;
            }
            (count as f64) <= capacity
        }
        Err(err) => {
            if fail_open {
                warn!(error = %err, key = %key, mode = "open", "rate_limit_store_error_policy_applied");
                true
            } else {
                warn!(error = %err, key = %key, mode = "closed", "rate_limit_store_error_policy_applied");
                false
            }
        }
    };

    if !allowed {
        warn!(
            client_ip = %client_ip,
            capacity,
            refill_per_second,
            limiter = "global_per_ip_token_bucket",
            "global_ip_rate_limiter_triggered"
        );
        return ApiError::new(
            ErrorCategory::Internal,
            "TOO_MANY_REQUESTS",
            "global per-ip rate limit exceeded",
        )
        .into_response();
    }

    next.run(req).await
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff: usize = left.len() ^ right.len();

    for i in 0..max_len {
        let l = left.get(i).copied().unwrap_or(0);
        let r = right.get(i).copied().unwrap_or(0);
        diff |= (l ^ r) as usize;
    }

    diff == 0
}

pub(crate) fn bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

pub(crate) fn is_admin_api_path(path: &str) -> bool {
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    if parts.next() != Some("api") {
        return false;
    }

    match parts.next() {
        Some("admin") => true,
        Some(version)
            if version.starts_with('v') && version[1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            parts.next() == Some("admin")
        }
        _ => false,
    }
}

/// Returns the value to use for `Access-Control-Allow-Origin`, or `None` if the request
/// origin is not whitelisted.
pub(crate) fn compute_cors_origin<'a>(
    request_origin: &'a str,
    allowed: &[String],
    allow_any_origin: bool,
) -> Option<&'a str> {
    if request_origin.is_empty() {
        return None;
    }
    if allow_any_origin && allowed.is_empty() {
        return Some("*");
    }
    if allowed.iter().any(|origin| origin == "*") {
        return Some("*");
    }
    if allowed.iter().any(|origin| origin == request_origin) {
        return Some(request_origin);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{Method, Request, StatusCode};
    use axum::Json;

    #[test]
    fn scope_claim_string_is_split() {
        let claims = JwtClaims {
            sub: Some("u1".to_string()),
            iss: Some("issuer".to_string()),
            aud: None,
            exp: None,
            jti: None,
            token_use: None,
            tid: None,
            tenant_id: None,
            scope: Some("users.read users.write".to_string()),
            scp: None,
            roles: None,
            role: None,
            extra: Default::default(),
        };

        let scopes = scopes_from_claims(&claims);
        assert_eq!(
            scopes,
            vec!["users.read".to_string(), "users.write".to_string()]
        );
    }

    #[test]
    fn role_claim_falls_back_to_single_role() {
        let claims = JwtClaims {
            sub: Some("u1".to_string()),
            iss: Some("issuer".to_string()),
            aud: None,
            exp: None,
            jti: None,
            token_use: None,
            tid: None,
            tenant_id: None,
            scope: None,
            scp: None,
            roles: None,
            role: Some("admin".to_string()),
            extra: Default::default(),
        };

        let roles = roles_from_claims(&claims);
        assert_eq!(roles, vec!["admin".to_string()]);
    }

    #[test]
    fn tenant_path_extraction_works() {
        assert_eq!(tenant_from_path("/api/tenants/t1/users"), Some("t1"));
        assert_eq!(tenant_from_path("/api/users/me"), None);
    }

    #[test]
    fn tenant_claim_falls_back_to_tid() {
        let claims = JwtClaims {
            sub: Some("u1".to_string()),
            iss: Some("issuer".to_string()),
            aud: None,
            exp: None,
            jti: None,
            token_use: None,
            tid: Some("tenant-a".to_string()),
            tenant_id: None,
            scope: None,
            scp: None,
            roles: None,
            role: None,
            extra: Default::default(),
        };

        assert_eq!(tenant_from_claims(&claims).as_deref(), Some("tenant-a"));
    }

    #[derive(Clone)]
    struct TestState {
        runtime: RuntimeState,
        dependencies: Vec<DependencyStatus>,
    }

    impl HasRuntimeState for TestState {
        fn runtime_state(&self) -> &RuntimeState {
            &self.runtime
        }
    }

    impl HasReadinessDependencies for TestState {
        fn readiness_dependencies(&self) -> Vec<DependencyStatus> {
            self.dependencies.clone()
        }
    }

    #[tokio::test]
    async fn readiness_non_critical_failure_is_degraded_but_available() {
        let state = TestState {
            runtime: RuntimeState::new(),
            dependencies: vec![DependencyStatus {
                name: "cache",
                ready: false,
                critical: false,
                latency_ms: Some(250),
                detail: Some("cache-timeout".to_string()),
            }],
        };

        let (status, Json(payload)) = readiness_with_dependencies::<TestState>(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(payload.status, "degraded");
    }

    #[test]
    fn cors_allow_headers_is_strict_allowlist() {
        assert_eq!(
            cors_allow_headers_value(),
            "authorization,content-type,x-request-id,x-trace-id"
        );
    }

    #[test]
    fn cors_allow_methods_is_reduced_surface() {
        assert_eq!(cors_allow_methods_value(), "GET,POST,OPTIONS");
    }

    #[test]
    fn csrf_cookie_token_extraction_works() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_static("foo=1; krab_csrf_token=abc123; bar=2"),
        );

        assert_eq!(csrf_cookie_token(&headers).as_deref(), Some("abc123"));
    }

    #[test]
    fn csrf_header_token_extraction_works() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::HeaderName::from_static("x-csrf-token"),
            axum::http::HeaderValue::from_static("abc123"),
        );

        assert_eq!(csrf_header_token(&headers).as_deref(), Some("abc123"));
    }

    /// The middleware extraction functions and the shared wire-contract
    /// constants in `crate::csrf` must agree. The wasm client half of
    /// `#[server]` only ever uses the constants, so this pins the server half
    /// to the same names — they cannot drift apart again.
    #[test]
    fn csrf_middleware_names_match_shared_constants() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            axum::http::HeaderValue::from_str(&format!(
                "{}=tok-from-cookie",
                crate::csrf::CSRF_COOKIE_NAME
            ))
            .expect("cookie header should build"),
        );
        headers.insert(
            axum::http::HeaderName::from_bytes(crate::csrf::CSRF_HEADER_NAME.as_bytes())
                .expect("header name should build"),
            axum::http::HeaderValue::from_static("tok-from-header"),
        );

        assert_eq!(
            csrf_cookie_token(&headers).as_deref(),
            Some("tok-from-cookie")
        );
        assert_eq!(
            csrf_header_token(&headers).as_deref(),
            Some("tok-from-header")
        );
    }

    /// The token endpoint must expose the token in its JSON body under the
    /// shared field name and set the shared cookie name — with `HttpOnly`, the
    /// body is the only channel through which a browser client can learn the
    /// token.
    #[tokio::test]
    async fn csrf_token_endpoint_body_and_cookie_use_shared_names() {
        let response = csrf_token_endpoint().await;

        let set_cookie = response
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .expect("endpoint must set the CSRF cookie")
            .to_str()
            .expect("cookie should be ascii")
            .to_string();
        assert!(
            set_cookie.starts_with(&format!("{}=", crate::csrf::CSRF_COOKIE_NAME)),
            "cookie {set_cookie:?} does not use the shared cookie name"
        );
        assert!(set_cookie.contains("HttpOnly"));

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body should read");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("body should be JSON");
        let token = parsed
            .get(crate::csrf::CSRF_TOKEN_JSON_FIELD)
            .and_then(|v| v.as_str())
            .expect("body must carry the token under the shared field name");
        assert!(
            set_cookie.contains(token),
            "cookie and body must carry the same token"
        );
    }

    #[test]
    fn extract_client_ip_falls_back_to_connect_info_when_proxy_headers_untrusted() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "203.0.113.10, 10.0.0.2")
            .header("x-real-ip", "203.0.113.11")
            .body(Body::empty())
            .expect("request should build");
        req.extensions_mut()
            .insert(axum::extract::connect_info::ConnectInfo(
                "198.51.100.20:443"
                    .parse::<std::net::SocketAddr>()
                    .expect("socket addr should parse"),
            ));

        assert_eq!(extract_client_ip(&req, false), "198.51.100.20");
    }

    #[test]
    fn extract_client_ip_prefers_leftmost_forwarded_value_when_trusted() {
        let req = Request::builder()
            .header("x-forwarded-for", "203.0.113.10, 10.0.0.2")
            .header("x-real-ip", "203.0.113.11")
            .body(Body::empty())
            .expect("request should build");

        assert_eq!(extract_client_ip(&req, true), "203.0.113.10");
    }

    #[test]
    fn extract_client_ip_falls_back_to_real_ip_when_forwarded_missing() {
        let req = Request::builder()
            .header("x-real-ip", "203.0.113.11")
            .body(Body::empty())
            .expect("request should build");

        assert_eq!(extract_client_ip(&req, true), "203.0.113.11");
    }

    #[test]
    fn extract_client_ip_returns_unknown_without_headers_or_connect_info() {
        let req = Request::builder()
            .body(Body::empty())
            .expect("request should build");

        assert_eq!(extract_client_ip(&req, false), "unknown");
    }

    #[test]
    fn unsafe_method_detection_is_strict() {
        assert!(is_unsafe_http_method(&Method::POST));
        assert!(is_unsafe_http_method(&Method::PUT));
        assert!(is_unsafe_http_method(&Method::PATCH));
        assert!(is_unsafe_http_method(&Method::DELETE));
        assert!(!is_unsafe_http_method(&Method::GET));
        assert!(!is_unsafe_http_method(&Method::HEAD));
        assert!(!is_unsafe_http_method(&Method::OPTIONS));
    }

    #[test]
    fn constant_time_eq_requires_exact_match() {
        assert!(constant_time_eq(b"Bearer abc", b"Bearer abc"));
        assert!(!constant_time_eq(b"Bearer abc", b"Bearer abd"));
        assert!(!constant_time_eq(b"Bearer abc", b"Bearer abcx"));
    }
}
