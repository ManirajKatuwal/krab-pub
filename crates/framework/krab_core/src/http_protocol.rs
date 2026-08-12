use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Method, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::warn;

use crate::http_auth::AuthContext;
use crate::http_error::{ApiError, ErrorCategory};
use crate::http_runtime::HasRuntimeState;
use crate::protocol::{ProtocolConfig, ProtocolKind};

pub async fn protocol_resolution_middleware<S>(
    State(state): State<S>,
    mut req: Request<Body>,
    next: Next,
) -> Response
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let config = state.runtime_state().protocol_config();
    // A route family whose protocol is disabled is rejected below by
    // `resolve_protocol_for_request` with PROTOCOL_NOT_SUPPORTED. It used to
    // pass through to the handler unresolved, which exposed disabled
    // protocol surfaces.

    if runtime_switch_header_rejected_by_default(&req, &config) {
        return ApiError::new(
            ErrorCategory::Validation,
            "BAD_REQUEST",
            "runtime protocol switching via x-krab-protocol is disabled",
        )
        .into_response();
    }

    let resolved = match resolve_protocol_for_request(&req, &config) {
        Ok(protocol) => protocol,
        Err(err) => return err.into_response(),
    };

    req.extensions_mut().insert(resolved);

    let mut response = next.run(req).await;
    // Mirror the resolved protocol into the response extensions so
    // observability layers that run OUTSIDE this middleware (metrics,
    // tracing — they must stay outside auth to count auth rejections) can
    // still label by protocol.
    response.extensions_mut().insert(resolved);
    let header_value = HeaderValue::from_str(resolved.as_str())
        .unwrap_or_else(|_| HeaderValue::from_static("unknown"));
    response
        .headers_mut()
        .insert(HeaderName::from_static("x-krab-protocol"), header_value);
    response
}

fn request_tenant_hint(req: &Request<Body>) -> Option<String> {
    if let Some(ctx) = req.extensions().get::<AuthContext>() {
        if let Some(tenant_id) = &ctx.tenant_id {
            let trimmed = tenant_id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // The header/query tenant hint is client-supplied and unauthenticated —
    // trusting it lets a caller pick another tenant's protocol policy. It is
    // therefore opt-in and off by default.
    if !crate::http::bool_env("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED", false) {
        return None;
    }

    if let Some(value) = req.headers().get("x-krab-tenant-id") {
        if let Ok(raw) = value.to_str() {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                warn!(
                    tenant_id = %trimmed,
                    source = "header",
                    "untrusted_tenant_hint_fallback_used"
                );
                return Some(trimmed.to_string());
            }
        }
    }

    let query = req.uri().query()?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        if key.eq_ignore_ascii_case("tenant_id") || key.eq_ignore_ascii_case("tid") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                warn!(
                    tenant_id = %trimmed,
                    source = "query",
                    "untrusted_tenant_hint_fallback_used"
                );
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

fn policy_allowed_protocols(
    method: &Method,
    path: &str,
    tenant_id: Option<&str>,
    config: &ProtocolConfig,
) -> Vec<ProtocolKind> {
    let mut allowed = config.enabled_protocols.clone();
    let operation = operation_label(method, path);

    if let Some(restricted) = config.policy.restricted_operations.get(operation) {
        if !restricted.is_empty() {
            allowed.retain(|candidate| restricted.contains(candidate));
        }
    }

    if let Some(tenant_id) = tenant_id {
        if let Some(tenant_allowed) = config.policy.tenant_overrides.get(tenant_id) {
            if !tenant_allowed.is_empty() {
                allowed.retain(|candidate| tenant_allowed.contains(candidate));
            }
        }
    }

    allowed
}

#[allow(clippy::result_large_err)]
pub(crate) fn resolve_protocol_for_request(
    req: &Request<Body>,
    config: &ProtocolConfig,
) -> Result<ProtocolKind, ApiError> {
    let method = req.method();
    let path = req.uri().path();
    let operation = operation_label(method, path);
    let tenant_id = request_tenant_hint(req);
    let allowed = policy_allowed_protocols(method, path, tenant_id.as_deref(), config);

    if allowed.is_empty() {
        return Err(ApiError::new(
            ErrorCategory::Validation,
            "INVALID_PROTOCOL_HEADER",
            "x-krab-protocol header contains invalid protocol",
        )
        .with_details(serde_json::json!({
            "operation": operation,
            "tenant_id": tenant_id,
        })));
    }

    if let Some(route_protocol) = route_family_protocol(path) {
        if allowed.contains(&route_protocol) {
            return Ok(route_protocol);
        }

        return Err(ApiError::new(
            ErrorCategory::Validation,
            "PROTOCOL_NOT_SUPPORTED",
            "Requested protocol is not enabled for this service",
        )
        .with_details(serde_json::json!({
            "operation": operation,
            "tenant_id": tenant_id,
            "candidate_protocol": route_protocol.as_str(),
            "allowed_protocols": allowed.iter().map(ProtocolKind::as_str).collect::<Vec<_>>(),
        })));
    }

    if config.allow_runtime_switch_header {
        if let Some(client_preference) = extract_protocol_preference(req) {
            if allowed.contains(&client_preference) {
                return Ok(client_preference);
            }

            return Err(ApiError::new(
                ErrorCategory::Authz,
                "PROTOCOL_RESTRICTED",
                "Requested protocol is restricted by operation or tenant policy",
            )
            .with_details(serde_json::json!({
                "operation": operation,
                "tenant_id": tenant_id,
                "candidate_protocol": client_preference.as_str(),
                "allowed_protocols": allowed.iter().map(ProtocolKind::as_str).collect::<Vec<_>>(),
            })));
        }
    }

    if allowed.contains(&config.default_protocol) {
        return Ok(config.default_protocol);
    }

    Ok(allowed[0])
}

pub fn extract_protocol_preference(req: &Request<Body>) -> Option<ProtocolKind> {
    if let Some(value) = req.headers().get("x-krab-protocol") {
        if let Ok(raw) = value.to_str() {
            if let Some(protocol) = ProtocolKind::parse(raw) {
                return Some(protocol);
            }
        }
    }

    let query = req.uri().query()?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        if key.eq_ignore_ascii_case("protocol") {
            if let Some(protocol) = ProtocolKind::parse(value) {
                return Some(protocol);
            }
        }
    }
    None
}

pub fn route_family_protocol(path: &str) -> Option<ProtocolKind> {
    if path == "/api/v1/graphql" || path.starts_with("/api/v1/graphql/") {
        return Some(ProtocolKind::Graphql);
    }
    if path == "/api/v1/rpc" || path.starts_with("/api/v1/rpc/") {
        return Some(ProtocolKind::Rpc);
    }
    if path == "/api/v1/users" || path.starts_with("/api/v1/users/") {
        return Some(ProtocolKind::Rest);
    }
    None
}

pub fn runtime_switch_header_rejected_by_default(
    req: &Request<Body>,
    config: &ProtocolConfig,
) -> bool {
    req.headers().contains_key("x-krab-protocol") && !config.allow_runtime_switch_header
}

pub(crate) fn resolved_protocol(req: &Request<Body>) -> Option<ProtocolKind> {
    req.extensions().get::<ProtocolKind>().copied()
}

pub(crate) fn protocol_label(req: &Request<Body>) -> &'static str {
    resolved_protocol(req)
        .map(|protocol| protocol.as_str())
        .unwrap_or("unknown")
}

pub(crate) fn operation_label(method: &Method, path: &str) -> &'static str {
    match (method, path) {
        (&Method::GET, "/api/v1/users/me") => "users.getMe",
        (&Method::POST, "/api/v1/graphql") => "users.graphql",
        (&Method::POST, "/api/v1/rpc") => "users.rpc",
        (&Method::POST, "/api/v1/auth/login") => "auth.login",
        (&Method::POST, "/api/v1/auth/refresh") => "auth.refresh",
        (&Method::POST, "/api/v1/auth/revoke") => "auth.revoke",
        (&Method::GET, "/api/v1/auth/jwks") => "auth.jwks",
        (&Method::GET, "/api/v1/auth/status") => "auth.status",
        (&Method::GET, "/api/v1/auth/capabilities") => "auth.capabilities",
        _ => "http.request",
    }
}

pub(crate) fn selection_source_label(req: &Request<Body>, path: &str) -> &'static str {
    if req.headers().contains_key("x-krab-protocol") {
        return "client";
    }
    if route_family_protocol(path).is_some() {
        return "policy";
    }
    "default"
}

pub(crate) fn protocol_metric_index(protocol: Option<ProtocolKind>) -> usize {
    match protocol {
        Some(ProtocolKind::Rest) => 0,
        Some(ProtocolKind::Graphql) => 1,
        Some(ProtocolKind::Rpc) => 2,
        None => 3,
    }
}

pub(crate) fn response_status_class_index(status_code: u16) -> Option<usize> {
    if (200..300).contains(&status_code) {
        Some(0)
    } else if (400..500).contains(&status_code) {
        Some(1)
    } else if (500..600).contains(&status_code) {
        Some(2)
    } else {
        None
    }
}

pub(crate) fn response_metric_slot(class_index: usize, protocol_index: usize) -> usize {
    class_index * 4 + protocol_index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_context_with_tenant(tenant: &str) -> AuthContext {
        AuthContext {
            subject: Some("user".to_string()),
            issuer: Some("issuer".to_string()),
            provider: Some("test".to_string()),
            token_id: None,
            token_use: None,
            tenant_id: Some(tenant.to_string()),
            scopes: vec![],
            roles: vec![],
        }
    }

    fn protocol_config_for_tests() -> ProtocolConfig {
        ProtocolConfig {
            exposure_mode: crate::protocol::ExposureMode::Multi,
            enabled_protocols: vec![ProtocolKind::Rest, ProtocolKind::Graphql, ProtocolKind::Rpc],
            default_protocol: ProtocolKind::Graphql,
            topology: crate::protocol::DeploymentTopology::SingleService,
            policy: crate::protocol::ProtocolPolicy::default(),
            allow_runtime_switch_header: false,
        }
    }

    #[test]
    fn protocol_resolution_rejects_route_family_disallowed_by_operation_policy() {
        let mut config = protocol_config_for_tests();
        config.policy.restricted_operations.insert(
            "users.graphql".to_string(),
            vec![ProtocolKind::Rest, ProtocolKind::Rpc],
        );

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/graphql")
            .body(Body::empty())
            .unwrap();

        let err = resolve_protocol_for_request(&req, &config)
            .expect_err("graphql route should be rejected when operation policy disallows graphql");
        assert_eq!(err.code, "PROTOCOL_NOT_SUPPORTED");
        assert!(err
            .message
            .contains("Requested protocol is not enabled for this service"));
    }

    #[test]
    fn protocol_resolution_applies_tenant_override_from_auth_context() {
        let mut config = protocol_config_for_tests();
        config
            .policy
            .tenant_overrides
            .insert("tenant-a".to_string(), vec![ProtocolKind::Rest]);

        let mut req = Request::builder()
            .method("GET")
            .uri("/api/v1/unknown")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(auth_context_with_tenant("tenant-a"));

        let resolved = resolve_protocol_for_request(&req, &config)
            .expect("tenant override should force rest for unknown route");
        assert_eq!(resolved, ProtocolKind::Rest);
    }

    #[test]
    fn protocol_resolution_rejects_client_preference_when_disallowed_by_tenant_override() {
        let mut config = protocol_config_for_tests();
        config.allow_runtime_switch_header = true;
        config
            .policy
            .tenant_overrides
            .insert("tenant-a".to_string(), vec![ProtocolKind::Rest]);

        let mut req = Request::builder()
            .method("GET")
            .uri("/api/v1/unknown")
            .header("x-krab-protocol", "graphql")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(auth_context_with_tenant("tenant-a"));

        let err = resolve_protocol_for_request(&req, &config)
            .expect_err("client preference should be rejected by tenant override");
        assert_eq!(err.code, "PROTOCOL_RESTRICTED");
        assert!(err
            .message
            .contains("Requested protocol is restricted by operation or tenant policy"));
    }

    /// The authenticated tenant claim must win over a spoofed
    /// `x-krab-tenant-id` header, even when the untrusted fallback knob is on.
    #[test]
    #[serial_test::serial]
    fn tenant_claim_beats_spoofed_header() {
        std::env::set_var("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED", "true");

        let mut config = protocol_config_for_tests();
        // tenant-a (from the auth context) is pinned to rest; tenant-b (the
        // spoofed header) is pinned to rpc. Resolution must follow tenant-a.
        config
            .policy
            .tenant_overrides
            .insert("tenant-a".to_string(), vec![ProtocolKind::Rest]);
        config
            .policy
            .tenant_overrides
            .insert("tenant-b".to_string(), vec![ProtocolKind::Rpc]);

        let mut req = Request::builder()
            .method("GET")
            .uri("/api/v1/unknown")
            .header("x-krab-tenant-id", "tenant-b")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(auth_context_with_tenant("tenant-a"));

        let resolved = resolve_protocol_for_request(&req, &config);
        std::env::remove_var("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED");

        assert_eq!(
            resolved.expect("resolution should succeed"),
            ProtocolKind::Rest,
            "the authenticated tenant's policy must apply, not the header's"
        );
    }

    /// Without the opt-in knob, the client-supplied tenant hint is ignored:
    /// the tenant override never applies and the default protocol wins.
    #[test]
    #[serial_test::serial]
    fn untrusted_tenant_hint_is_ignored_by_default() {
        std::env::remove_var("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED");

        let mut config = protocol_config_for_tests();
        config
            .policy
            .tenant_overrides
            .insert("tenant-a".to_string(), vec![ProtocolKind::Rest]);

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/unknown?tenant_id=tenant-a")
            .header("x-krab-tenant-id", "tenant-a")
            .body(Body::empty())
            .unwrap();

        let resolved = resolve_protocol_for_request(&req, &config)
            .expect("resolution should fall back to the default protocol");
        assert_eq!(
            resolved,
            ProtocolKind::Graphql,
            "unauthenticated tenant hints must not select tenant policy by default"
        );
    }

    /// With `KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED=true` and no auth context,
    /// the legacy header fallback still works for operators that opt in.
    #[test]
    #[serial_test::serial]
    fn untrusted_tenant_hint_applies_with_opt_in_knob() {
        std::env::set_var("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED", "true");

        let mut config = protocol_config_for_tests();
        config
            .policy
            .tenant_overrides
            .insert("tenant-a".to_string(), vec![ProtocolKind::Rest]);

        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/unknown")
            .header("x-krab-tenant-id", "tenant-a")
            .body(Body::empty())
            .unwrap();

        let resolved = resolve_protocol_for_request(&req, &config);
        std::env::remove_var("KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED");

        assert_eq!(
            resolved.expect("resolution should succeed"),
            ProtocolKind::Rest
        );
    }

    #[test]
    fn protocol_resolution_rejects_route_family_when_protocol_is_disabled() {
        let config = ProtocolConfig {
            exposure_mode: crate::protocol::ExposureMode::Single,
            enabled_protocols: vec![ProtocolKind::Rest],
            default_protocol: ProtocolKind::Rest,
            topology: crate::protocol::DeploymentTopology::SingleService,
            policy: crate::protocol::ProtocolPolicy::default(),
            allow_runtime_switch_header: false,
        };

        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/graphql")
            .body(Body::empty())
            .unwrap();

        let err = resolve_protocol_for_request(&req, &config)
            .expect_err("graphql route should be rejected when graphql is disabled");
        assert_eq!(err.code, "PROTOCOL_NOT_SUPPORTED");
    }
}
