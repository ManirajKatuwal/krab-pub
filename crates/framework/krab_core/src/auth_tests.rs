#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::{json, Value};
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use tower::ServiceExt;

    use crate::http::{apply_common_http_layers, OverloadMode, RuntimeState};

    #[derive(Clone)]
    struct TestState {
        runtime: RuntimeState,
    }

    impl crate::http::HasRuntimeState for TestState {
        fn runtime_state(&self) -> &RuntimeState {
            &self.runtime
        }
    }

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn reset_auth_env() {
        for key in [
            "KRAB_ENVIRONMENT",
            "KRAB_REDIS_URL",
            "KRAB_AUTH_MODE",
            "KRAB_JWT_SECRET",
            "KRAB_JWT_SECRET_FILE",
            "KRAB_JWT_KEYS_JSON",
            "KRAB_JWT_KEYS_JSON_FILE",
            "KRAB_JWT_PROVIDERS_JSON",
            "KRAB_JWT_PROVIDERS_JSON_FILE",
            "KRAB_OIDC_ISSUER",
            "KRAB_OIDC_AUDIENCE",
            "KRAB_JWT_ALLOWED_ALGS",
            "KRAB_AUTH_REQUIRED_SCOPES",
            "KRAB_AUTH_REQUIRED_ROLES",
            "KRAB_AUTH_REQUIRED_CLAIMS_JSON",
            "KRAB_AUTH_ROUTE_POLICIES_JSON",
            "KRAB_AUTH_REQUIRE_TENANT_CLAIM",
            "KRAB_AUTH_REQUIRE_TENANT_MATCH",
            "KRAB_JWT_REQUIRE_KID",
            "KRAB_AUTH_OPEN_PATHS",
            "KRAB_AUTH_PUBLIC_PATHS",
            "KRAB_METRICS_PUBLIC",
            "KRAB_TRUST_PROXY_HEADERS",
            "KRAB_TRUSTED_PROXY_HOPS",
            "KRAB_RATE_LIMIT_CAPACITY",
            "KRAB_RATE_LIMIT_REFILL_PER_SEC",
            "KRAB_RATE_LIMIT_FAIL_OPEN",
            "KRAB_AUTH_FAILURE_WINDOW_SECS",
            "KRAB_AUTH_FAILURE_THRESHOLD",
            "KRAB_HTTP_REQUEST_TIMEOUT_SECS",
            "KRAB_HTTP_MAX_CONCURRENCY",
            "KRAB_HTTP_OVERLOAD_MODE",
            "KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED",
            "KRAB_OIDC_JWKS_URL",
            "KRAB_OIDC_JWKS_MIN_REFETCH_SECS",
            "KRAB_JWT_KEY_NOT_AFTER_JSON",
            "KRAB_BEARER_TOKEN_FILE",
        ] {
            std::env::remove_var(key);
        }

        // Keep auth-focused tests deterministic by preventing global request
        // limiter interference when many tests run in one process.
        std::env::set_var("KRAB_ENVIRONMENT", "dev");
        std::env::set_var("KRAB_RATE_LIMIT_CAPACITY", "100000");
        std::env::set_var("KRAB_RATE_LIMIT_REFILL_PER_SEC", "100000");
        std::env::set_var("KRAB_RATE_LIMIT_FAIL_OPEN", "true");
        std::env::set_var("KRAB_TRUST_PROXY_HEADERS", "true");
    }

    fn test_app_with_state(state: TestState) -> Router {
        let app = Router::new()
            .route("/protected", axum::routing::get(|| async { "ok" }))
            .route("/api/admin/audit", axum::routing::get(|| async { "admin" }))
            .route(
                "/api/tenants/{tenant_id}/users",
                axum::routing::get(|| async { "tenant" }),
            );

        apply_common_http_layers(app, state.clone()).with_state(state)
    }

    fn test_app() -> Router {
        test_app_with_state(TestState {
            runtime: RuntimeState::new(),
        })
    }

    fn test_app_and_state() -> (Router, TestState) {
        let state = TestState {
            runtime: RuntimeState::new(),
        };
        (test_app_with_state(state.clone()), state)
    }

    fn generate_token(claims: Value) -> String {
        let key = b"secret";
        encode(&Header::default(), &claims, &EncodingKey::from_secret(key)).unwrap()
    }

    fn generate_token_with_kid(kid: &str, claims: Value, secret: &[u8]) -> String {
        let header = Header {
            kid: Some(kid.to_string()),
            ..Default::default()
        };
        encode(&header, &claims, &EncodingKey::from_secret(secret)).unwrap()
    }

    #[tokio::test]
    #[serial]
    async fn test_expired_token() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "exp": 1000000000 // Past timestamp
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_wrong_issuer() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_OIDC_ISSUER", "correct-issuer");

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "iss": "wrong-issuer",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_wrong_audience() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "correct-aud");

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "aud": "wrong-aud",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_missing_required_scope() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_REQUIRED_SCOPES", "read:data");

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "scope": "other:scope",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.4")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_revoked_key() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        // We set keys JSON where 'kid1' is valid but we sign with 'kid2' which is not in the set
        std::env::set_var("KRAB_JWT_KEYS_JSON", r#"{"kid1": "secret1"}"#);

        let app = test_app();

        let claims = json!({
            "sub": "user",
            "exp": 9999999999i64
        });

        // Sign with a secret that corresponds to kid2, but kid2 is not in the trusted set
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"secret2"),
        )
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should fail because kid2 is not found in loaded keys (effectively revoked/unknown)
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_multi_provider_jwks_selection() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var(
            "KRAB_JWT_PROVIDERS_JSON",
            r#"[
                {"name":"provider-a","issuer":"iss-a","audience":"aud-a","keys":{"kid-a":"secret-a"}},
                {"name":"provider-b","issuer":"iss-b","audience":"aud-b","keys":{"kid-b":"secret-b"}}
            ]"#,
        );

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "iss": "iss-b",
            "aud": "aud-b",
            "exp": 9999999999i64
        });
        let token = generate_token_with_kid("kid-b", claims, b"secret-b");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.6")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[serial]
    async fn test_tenant_path_mismatch_is_denied() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_REQUIRE_TENANT_MATCH", "true");

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "tenant_id": "tenant-a",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tenants/tenant-b/users")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_shared_jwt_validation_reads_secret_file() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");

        let secret_path = std::env::current_dir()
            .expect("current dir should resolve")
            .join(format!(
                "krab_test_jwt_secret_{}_{}.txt",
                std::process::id(),
                1
            ));
        std::fs::write(&secret_path, "secret\n").expect("secret file should be written");
        std::env::set_var(
            "KRAB_JWT_SECRET_FILE",
            secret_path.to_string_lossy().to_string(),
        );

        let app = test_app();
        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        std::env::remove_var("KRAB_JWT_SECRET_FILE");
        let _ = std::fs::remove_file(secret_path);
    }

    #[tokio::test]
    #[serial]
    async fn test_refresh_token_is_rejected_for_route_auth() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let app = test_app();
        let token = generate_token(json!({
            "sub": "user",
            "token_use": "refresh",
            "jti": "refresh-jti",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_revoked_access_token_is_rejected() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let (app, state) = test_app_and_state();
        state
            .runtime
            .store
            .set("auth:revoked:access-jti", "1", Duration::from_secs(60))
            .await
            .expect("revocation marker should be stored");

        let token = generate_token(json!({
            "sub": "user",
            "token_use": "access",
            "jti": "access-jti",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.11")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[serial]
    async fn test_hs256_is_allowed_in_non_dev_when_explicitly_allowlisted() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_JWT_ALLOWED_ALGS", "HS256");

        let app = test_app();
        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.12")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[serial]
    async fn test_route_policy_requires_composed_scope() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var(
            "KRAB_AUTH_ROUTE_POLICIES_JSON",
            r#"[{"prefix":"/api/admin","all_scopes":["audit.read"]}]"#,
        );

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "scope": "users.read",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/audit")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A typo in `KRAB_AUTH_ROUTE_POLICIES_JSON` used to parse to an empty
    /// policy set — every configured restriction silently vanished, fail-open.
    /// A configured-but-unparseable policy set must reject the request.
    #[tokio::test]
    #[serial]
    async fn test_malformed_route_policy_json_fails_closed() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var(
            "KRAB_AUTH_ROUTE_POLICIES_JSON",
            // Trailing comma makes this invalid JSON. The prefix deliberately
            // does not match the request path: the parse failure alone must
            // reject, before any prefix filtering.
            r#"[{"prefix":"/api/reports","all_scopes":["audit.read"],}]"#,
        );

        let app = test_app();
        let claims = json!({
            "sub": "user",
            "scope": "audit.read",
            "exp": 9999999999i64
        });
        let token = generate_token(claims);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// `service_auth_middleware` reads the `AuthContext` extension that
    /// `auth_middleware` inserts. The layers used to run in the wrong order
    /// (scope check before auth), so a request with a perfectly valid token
    /// carrying the service scope still got 403 on every `/internal` route.
    /// This drives a request through the full `apply_common_http_layers`
    /// stack, not a hand-assembled router, so the real ordering is what is
    /// under test.
    #[tokio::test]
    #[serial]
    async fn test_internal_route_reachable_with_service_scope() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let state = TestState {
            runtime: RuntimeState::new(),
        };
        let app = apply_common_http_layers(
            Router::new().route(
                "/internal/replicate",
                axum::routing::get(|| async { "internal-ok" }),
            ),
            state.clone(),
        )
        .with_state(state.clone());

        let scope = state.runtime.service_auth_scope.clone();
        let token = generate_token(json!({
            "sub": "service-caller",
            "scope": scope,
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/internal/replicate")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.30")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a valid token carrying the service scope must reach /internal routes"
        );
    }

    /// The service-scope gate must still hold: a token that authenticates but
    /// lacks the service scope is 403 on `/internal` routes.
    #[tokio::test]
    #[serial]
    async fn test_internal_route_forbidden_without_service_scope() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let state = TestState {
            runtime: RuntimeState::new(),
        };
        let app = apply_common_http_layers(
            Router::new().route(
                "/internal/replicate",
                axum::routing::get(|| async { "internal-ok" }),
            ),
            state.clone(),
        )
        .with_state(state);

        let token = generate_token(json!({
            "sub": "user",
            "scope": "users.read",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/internal/replicate")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.31")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// An allowlist mixing HMAC with asymmetric families is the algorithm
    /// confusion footgun; the request path must fail closed rather than
    /// verify anything under it.
    #[tokio::test]
    #[serial]
    async fn test_mixed_algorithm_family_allowlist_fails_closed() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_JWT_ALLOWED_ALGS", "HS256,RS256");

        let app = test_app();
        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.40")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// `/metrics` is closed by default, and an explicit open-path list that
    /// omits it must keep it closed too — `KRAB_AUTH_OPEN_PATHS` replaces the
    /// whole list when set, and only `KRAB_METRICS_PUBLIC` reopens metrics.
    #[tokio::test]
    #[serial]
    async fn test_open_paths_env_can_close_metrics() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/health,/ready");

        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .header("x-forwarded-for", "10.10.0.50")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "with an explicit open-path list omitting it, /metrics must require auth"
        );
    }

    async fn anonymous_status(app: &Router, path: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("x-forwarded-for", "10.10.0.51")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// An operator's explicit `KRAB_AUTH_OPEN_PATHS` is the complete open
    /// list: public paths a service declares in code must not reopen a route
    /// it omits. `KRAB_AUTH_PUBLIC_PATHS` stays additive.
    #[tokio::test]
    #[serial]
    async fn explicit_open_paths_are_not_reopened_by_code_declared_public_paths() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/health,/ready");
        std::env::set_var("KRAB_AUTH_PUBLIC_PATHS", "/api/tenants/*");

        let runtime = RuntimeState::new().with_public_paths(["/protected"]);
        let app = test_app_with_state(TestState { runtime });
        let closed = anonymous_status(&app, "/protected").await;
        let operator_public = anonymous_status(&app, "/api/tenants/acme/users").await;
        std::env::remove_var("KRAB_AUTH_PUBLIC_PATHS");

        assert_eq!(
            closed,
            StatusCode::UNAUTHORIZED,
            "a code-declared public path must not reopen a route the operator's \
             explicit KRAB_AUTH_OPEN_PATHS omits"
        );
        assert_eq!(operator_public, StatusCode::OK);
    }

    /// Without an explicit open-path list, code-declared public paths are
    /// open, as before.
    #[tokio::test]
    #[serial]
    async fn code_declared_public_paths_apply_without_explicit_open_paths() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let runtime = RuntimeState::new().with_public_paths(["/protected"]);
        let app = test_app_with_state(TestState { runtime });
        assert_eq!(anonymous_status(&app, "/protected").await, StatusCode::OK);
    }

    fn multi_protocol_config() -> crate::protocol::ProtocolConfig {
        crate::protocol::ProtocolConfig {
            exposure_mode: crate::protocol::ExposureMode::Multi,
            enabled_protocols: vec![
                crate::protocol::ProtocolKind::Rest,
                crate::protocol::ProtocolKind::Graphql,
                crate::protocol::ProtocolKind::Rpc,
            ],
            default_protocol: crate::protocol::ProtocolKind::Rest,
            topology: crate::protocol::DeploymentTopology::SingleService,
            policy: crate::protocol::ProtocolPolicy::default(),
            allow_runtime_switch_header: false,
        }
    }

    /// A route family whose protocol is disabled used to pass through the
    /// protocol middleware unresolved, exposing the disabled surface. It must
    /// now be rejected with PROTOCOL_NOT_SUPPORTED.
    #[tokio::test]
    #[serial]
    async fn test_disabled_route_family_is_rejected() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let config = crate::protocol::ProtocolConfig {
            exposure_mode: crate::protocol::ExposureMode::Single,
            enabled_protocols: vec![crate::protocol::ProtocolKind::Rest],
            default_protocol: crate::protocol::ProtocolKind::Rest,
            topology: crate::protocol::DeploymentTopology::SingleService,
            policy: crate::protocol::ProtocolPolicy::default(),
            allow_runtime_switch_header: false,
        };
        let state = TestState {
            runtime: RuntimeState::new().with_protocol_config(config),
        };
        let app = apply_common_http_layers(
            Router::new().route("/api/v1/graphql", axum::routing::post(|| async { "gql" })),
            state.clone(),
        )
        .with_state(state);

        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/graphql")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.60")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a disabled route family must be rejected, not passed through"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed.get("code").and_then(|v| v.as_str()),
            Some("PROTOCOL_NOT_SUPPORTED")
        );
    }

    /// Protocol resolution now runs INSIDE auth (it needs AuthContext for
    /// tenant policy), while metrics stays outside auth. Metrics must still
    /// label requests with the resolved protocol, learned from the response
    /// extension the protocol middleware mirrors back.
    #[tokio::test]
    #[serial]
    async fn test_metrics_label_resolved_protocol_after_reorder() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let state = TestState {
            runtime: RuntimeState::new().with_protocol_config(multi_protocol_config()),
        };
        let app = apply_common_http_layers(
            Router::new().route("/api/v1/graphql", axum::routing::post(|| async { "gql" })),
            state.clone(),
        )
        .with_state(state.clone());

        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/graphql")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.61")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-krab-protocol")
                .and_then(|v| v.to_str().ok()),
            Some("graphql")
        );
        // Index 1 is graphql; index 3 is "unknown", which is where the count
        // would land if metrics could no longer see the resolved protocol.
        assert_eq!(
            state.runtime.protocol_request_totals[1].load(std::sync::atomic::Ordering::Relaxed),
            1,
            "metrics must label the request as graphql"
        );
        assert_eq!(
            state.runtime.protocol_request_totals[3].load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the request must not be counted as protocol=unknown"
        );
    }

    /// The 429 from the global rate limiter must carry the new
    /// `rate_limited` wire category.
    #[tokio::test]
    #[serial]
    async fn test_rate_limited_response_body_category() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_RATE_LIMIT_CAPACITY", "1");
        std::env::set_var("KRAB_RATE_LIMIT_REFILL_PER_SEC", "1");

        let app = test_app();

        // Capacity 1 and a 1-second window: of three back-to-back requests
        // from the same IP, at least two share a window, so at least one is
        // rate limited even if a window boundary is crossed mid-test.
        let mut limited_body = None;
        for _ in 0..3 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/health")
                        .header("x-forwarded-for", "10.10.9.9")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                limited_body = Some(body);
                break;
            }
        }

        let body = limited_body.expect("one of three requests must be rate limited");
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed.get("category").and_then(|v| v.as_str()),
            Some("rate_limited")
        );
        assert_eq!(
            parsed.get("code").and_then(|v| v.as_str()),
            Some("TOO_MANY_REQUESTS")
        );
    }

    /// A handler that overruns `KRAB_HTTP_REQUEST_TIMEOUT_SECS` must produce
    /// a timeout status, and the shed response must still pass through the
    /// security-headers layer.
    #[tokio::test]
    #[serial]
    async fn test_request_timeout_produces_408_with_security_headers() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/slow");
        std::env::set_var("KRAB_HTTP_REQUEST_TIMEOUT_SECS", "1");

        let state = TestState {
            runtime: RuntimeState::new(),
        };
        let app = apply_common_http_layers(
            Router::new().route(
                "/slow",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "too-late"
                }),
            ),
            state.clone(),
        )
        .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/slow")
                    .header("x-forwarded-for", "10.10.0.62")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(
            response
                .headers()
                .get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
            "timeout responses must still pass through the security-headers layer"
        );
    }

    /// With `KRAB_HTTP_MAX_CONCURRENCY=1`, two concurrent requests to a slow
    /// handler are serialized by the concurrency limit.
    #[tokio::test]
    #[serial]
    async fn test_concurrency_limit_serializes_requests() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/slow");
        std::env::set_var("KRAB_HTTP_MAX_CONCURRENCY", "1");

        let state = TestState {
            runtime: RuntimeState::new(),
        };
        let app = apply_common_http_layers(
            Router::new().route(
                "/slow",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    "done"
                }),
            ),
            state.clone(),
        )
        .with_state(state);

        let request = |ip: &'static str| {
            let app = app.clone();
            async move {
                app.oneshot(
                    Request::builder()
                        .uri("/slow")
                        .header("x-forwarded-for", ip)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
            }
        };

        let started = std::time::Instant::now();
        let (first, second) = tokio::join!(request("10.10.0.63"), request("10.10.0.64"));
        let elapsed = started.elapsed();

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert!(
            elapsed >= Duration::from_millis(550),
            "with max concurrency 1 the two 300ms requests must run serially, took {elapsed:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_load_shed_mode_drops_excess_requests_with_503() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/slow_shed");
        std::env::set_var("KRAB_HTTP_MAX_CONCURRENCY", "1");
        std::env::set_var("KRAB_HTTP_OVERLOAD_MODE", "shed");

        let entered = Arc::new(Notify::new());
        let entered_clone = entered.clone();
        let release = Arc::new(Notify::new());
        let release_clone = release.clone();

        let state = TestState {
            runtime: RuntimeState::new(),
        };
        let app = apply_common_http_layers(
            Router::new().route(
                "/slow_shed",
                axum::routing::get(move || {
                    let entered = entered_clone.clone();
                    let release = release_clone.clone();
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        "done"
                    }
                }),
            ),
            state.clone(),
        )
        .with_state(state);

        let notify_enter_wait = entered.notified();
        let app1 = app.clone();
        let handle1 = tokio::spawn(async move {
            app1.oneshot(
                Request::builder()
                    .uri("/slow_shed")
                    .header("x-forwarded-for", "10.10.0.65")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        });

        // Wait until request 1 is holding the single permit inside the handler
        notify_enter_wait.await;

        let app2 = app.clone();
        let res2 = app2
            .oneshot(
                Request::builder()
                    .uri("/slow_shed")
                    .header("x-forwarded-for", "10.10.0.66")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Release request 1
        release.notify_one();
        let res1 = handle1.await.unwrap();

        assert_eq!(res1.status(), StatusCode::OK);
        // 503 and not 429: shed mode reports that the *service* is out of
        // capacity, which is what `.env.example` and
        // `docs/reference/environment.md` promise and what LB and alert
        // policies match on. `KRAB_HTTP_OVERLOAD_MODE` is cleared by
        // `reset_auth_env`, so a failure here cannot leak shed mode into the
        // serial tests that follow.
        assert_eq!(res2.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    #[serial]
    fn test_overload_mode_env_is_trimmed_and_validated() {
        let _guard = env_lock();
        reset_auth_env();

        assert_eq!(crate::http::overload_mode_from_env(), OverloadMode::Queue);

        for raw in ["shed", " shed ", "SHED", "\t Shed\n"] {
            std::env::set_var("KRAB_HTTP_OVERLOAD_MODE", raw);
            assert_eq!(
                crate::http::overload_mode_from_env(),
                OverloadMode::Shed,
                "{raw:?} must select shed mode"
            );
        }

        // Unknown values fall back to queue rather than to shed: a typo must
        // not turn shedding on, and it must not turn it off silently either —
        // the fallback logs `env_value_invalid_using_default`.
        for raw in ["queue", " ", "", "sched", "drop"] {
            std::env::set_var("KRAB_HTTP_OVERLOAD_MODE", raw);
            assert_eq!(
                crate::http::overload_mode_from_env(),
                OverloadMode::Queue,
                "{raw:?} must fall back to queue mode"
            );
        }

        reset_auth_env();
    }

    /// The JWT verifier cache is built once per `RuntimeState`: rotating the
    /// env secret mid-flight must not affect an existing state (no
    /// per-request provider reload), while a freshly built state picks up
    /// the new secret.
    #[tokio::test]
    #[serial]
    async fn test_jwt_verifier_cache_reused_across_requests() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let (app, _state) = test_app_and_state();
        let token = generate_token(json!({
            "sub": "user",
            "exp": 9999999999i64
        }));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.65")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Rotate the env secret. The existing state's cache must keep
        // verifying with the material it was built from.
        std::env::set_var("KRAB_JWT_SECRET", "rotated-secret");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.66")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the cached provider must be reused; a per-request env reload would reject this token"
        );

        // A fresh state (fresh cache) sees the rotated secret and rejects
        // the old token.
        let fresh_app = test_app();
        let response = fresh_app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {}", token))
                    .header("x-forwarded-for", "10.10.0.67")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Metrics used to be on the default open-path list, so an unconfigured
    /// service published its route inventory, traffic shape, error counts and
    /// latency histograms to anyone who asked. The default is now closed.
    #[tokio::test]
    #[serial]
    async fn test_metrics_requires_auth_by_default() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        for uri in ["/metrics", "/metrics/prometheus"] {
            let app = test_app();
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("x-forwarded-for", "10.10.0.51")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} must not be anonymously reachable without an explicit opt-in"
            );
        }
    }

    /// The documented single-step restore for anyone scraping the old default.
    #[tokio::test]
    #[serial]
    async fn test_metrics_public_env_restores_anonymous_scraping() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_METRICS_PUBLIC", "true");

        for uri in ["/metrics", "/metrics/prometheus"] {
            let app = test_app();
            let response = app
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("x-forwarded-for", "10.10.0.52")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            // The test router has no metrics route; the point is that the auth
            // layer passes the request through (404 from routing, not 401).
            assert_ne!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "KRAB_METRICS_PUBLIC=true must reopen {uri}"
            );
        }

        std::env::remove_var("KRAB_METRICS_PUBLIC");
    }

    #[tokio::test]
    #[serial]
    async fn test_auth_failure_threshold_env_is_configurable() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        // Pin the window well beyond the test's runtime. With the 60s default
        // an epoch boundary between the 3rd and 4th request would reset the
        // counter and flip the 429 assertion below to 401.
        std::env::set_var("KRAB_AUTH_FAILURE_WINDOW_SECS", "3600");
        std::env::set_var("KRAB_AUTH_FAILURE_THRESHOLD", "3");

        let app = test_app();

        let make_req = |ip: &str| {
            Request::builder()
                .uri("/protected")
                .header("Authorization", "Bearer invalid-token")
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap()
        };

        // First 3 failures from 10.10.0.1 return 401 (accumulating up to threshold).
        for _ in 0..3 {
            let res = app.clone().oneshot(make_req("10.10.0.1")).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        // 4th failure exceeds threshold (3) -> returns 429 Too Many Requests.
        let res = app.clone().oneshot(make_req("10.10.0.1")).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

        // A different client IP has not reached the threshold and still gets 401.
        let res = app.clone().oneshot(make_req("10.10.0.2")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        std::env::remove_var("KRAB_AUTH_FAILURE_WINDOW_SECS");
        std::env::remove_var("KRAB_AUTH_FAILURE_THRESHOLD");
    }

    #[tokio::test]
    #[serial]
    async fn test_auth_failure_window_rolls_over() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_FAILURE_WINDOW_SECS", "2");
        std::env::set_var("KRAB_AUTH_FAILURE_THRESHOLD", "1");

        let app = test_app();

        let make_req = || {
            Request::builder()
                .uri("/protected")
                .header("Authorization", "Bearer invalid-token")
                .header("x-forwarded-for", "10.10.0.3")
                .body(Body::empty())
                .unwrap()
        };

        // 1st failure -> 401 (threshold is 1)
        let res = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // 2nd failure immediately -> 429 (count is 2 > threshold 1)
        let res = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);

        // Sleep longer than the 2-second window duration to ensure epoch rollover.
        tokio::time::sleep(Duration::from_millis(2200)).await;

        // After window rollover, a new window counter starts -> 401.
        let res = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        std::env::remove_var("KRAB_AUTH_FAILURE_WINDOW_SECS");
        std::env::remove_var("KRAB_AUTH_FAILURE_THRESHOLD");
    }

    /// A token with no `kid` is tried against every configured key, so one
    /// signed with the non-default key during a rotation still verifies.
    #[tokio::test]
    #[serial]
    async fn kidless_token_signed_with_a_non_default_key_is_accepted() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var(
            "KRAB_JWT_KEYS_JSON",
            r#"{"default": "old-secret", "next": "new-secret"}"#,
        );

        let app = test_app();
        let claims = json!({"sub": "user", "exp": 9999999999i64});
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"new-secret"),
        )
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("x-forwarded-for", "10.10.0.40")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// A kid-less token is tried against at most `MAX_KIDLESS_KEY_TRIALS`
    /// keys: each trial is a signature verification, so an unbounded trial
    /// is a CPU amplifier.
    #[tokio::test]
    #[serial]
    async fn kidless_token_key_trials_are_capped() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        let keys: serde_json::Map<String, Value> = (0..10)
            .map(|i| (format!("k{i:02}"), json!(format!("secret-{i:02}"))))
            .collect();
        std::env::set_var("KRAB_JWT_KEYS_JSON", Value::Object(keys).to_string());
        assert_eq!(crate::http_auth::MAX_KIDLESS_KEY_TRIALS, 8);

        let app = test_app();
        let kidless = |secret: &str| {
            encode(
                &Header::default(),
                &json!({"sub": "user", "exp": 9999999999i64}),
                &EncodingKey::from_secret(secret.as_bytes()),
            )
            .unwrap()
        };
        // Keys are tried in id order; k07 is the 8th, k08 the 9th.
        let within = call(&app, &kidless("secret-07"), "10.10.0.44").await;
        let beyond = call(&app, &kidless("secret-08"), "10.10.0.45").await;
        assert_eq!(within, StatusCode::OK);
        assert_eq!(beyond, StatusCode::UNAUTHORIZED);
    }

    /// An address already over its auth-failure budget is answered 429
    /// before its token is verified — even a valid one — instead of costing
    /// a verification per request.
    #[tokio::test]
    #[serial]
    async fn over_budget_address_is_rejected_before_verification() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_FAILURE_WINDOW_SECS", "3600");
        std::env::set_var("KRAB_AUTH_FAILURE_THRESHOLD", "2");

        let (app, state) = test_app_and_state();
        let claims = json!({"sub": "user", "exp": 9999999999i64});
        // Kid-less and signed with the wrong key: the amplifying shape.
        let kidless_bad = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"not-the-secret"),
        )
        .unwrap();
        // A valid token naming its key (KRAB_JWT_SECRET is kid `default`).
        let valid_with_kid = generate_token_with_kid("default", claims, b"secret");

        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(call(&app, &kidless_bad, "10.10.0.46").await);
        }
        let reasons_before: u64 = state
            .runtime
            .auth_failure_reasons
            .iter()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .sum();
        let kidless_from_blocked = call(&app, &kidless_bad, "10.10.0.46").await;
        let reasons_after: u64 = state
            .runtime
            .auth_failure_reasons
            .iter()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .sum();
        let valid_with_kid_from_blocked = call(&app, &valid_with_kid, "10.10.0.46").await;
        std::env::remove_var("KRAB_AUTH_FAILURE_WINDOW_SECS");
        std::env::remove_var("KRAB_AUTH_FAILURE_THRESHOLD");

        assert_eq!(
            statuses,
            vec![
                StatusCode::UNAUTHORIZED,
                StatusCode::UNAUTHORIZED,
                StatusCode::TOO_MANY_REQUESTS
            ]
        );
        assert_eq!(kidless_from_blocked, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            reasons_after, reasons_before,
            "a kid-less token from an over-budget address must not reach verification"
        );
        // A valid token that names its key is never locked out by other
        // callers' failures on the same address (shared NAT).
        assert_eq!(valid_with_kid_from_blocked, StatusCode::OK);
    }

    /// `KRAB_JWT_REQUIRE_KID` still refuses a kid-less token outright.
    #[tokio::test]
    #[serial]
    async fn kidless_token_is_rejected_when_kid_is_required() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_REQUIRE_KID", "true");
        std::env::set_var("KRAB_JWT_KEYS_JSON", r#"{"default": "s1", "next": "s2"}"#);

        let app = test_app();
        let token = encode(
            &Header::default(),
            &json!({"sub": "user", "exp": 9999999999i64}),
            &EncodingKey::from_secret(b"s2"),
        )
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("x-forwarded-for", "10.10.0.41")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A store whose counters always fail, as during a Redis outage.
    struct CounterOutageStore(crate::store::MemoryStore);

    #[async_trait::async_trait]
    impl crate::store::DistributedStore for CounterOutageStore {
        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            self.0.get(key).await
        }
        async fn set(&self, key: &str, value: &str, ttl: Duration) -> anyhow::Result<()> {
            self.0.set(key, value, ttl).await
        }
        async fn incr(&self, _key: &str, _delta: u64) -> anyhow::Result<u64> {
            anyhow::bail!("store unavailable")
        }
        async fn incr_with_ttl(
            &self,
            _key: &str,
            _delta: u64,
            _ttl: Duration,
        ) -> anyhow::Result<u64> {
            anyhow::bail!("store unavailable")
        }
        async fn expire(&self, key: &str, ttl: Duration) -> anyhow::Result<()> {
            self.0.expire(key, ttl).await
        }
        async fn delete(&self, key: &str) -> anyhow::Result<bool> {
            self.0.delete(key).await
        }
        async fn keys_with_prefix(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
            self.0.keys_with_prefix(prefix).await
        }
        async fn set_if_absent(
            &self,
            key: &str,
            value: &str,
            ttl: Duration,
        ) -> anyhow::Result<bool> {
            self.0.set_if_absent(key, value, ttl).await
        }
    }

    /// When the failure counter cannot be written the request still fails
    /// closed, but as 503 — an outage — not 429, which told clients and
    /// dashboards they were being rate limited.
    #[tokio::test]
    #[serial]
    async fn auth_failure_limiter_store_outage_is_503_not_429() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");

        let mut runtime = RuntimeState::new();
        runtime.store = std::sync::Arc::new(CounterOutageStore(crate::store::MemoryStore::new()));
        let app = test_app_with_state(TestState { runtime });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", "Bearer not-a-jwt")
                    .header("x-forwarded-for", "10.10.0.42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// `KRAB_BEARER_TOKEN_FILE` is honoured like every other secret's `_FILE`
    /// form; it used to be ignored and static mode answered 503.
    #[tokio::test]
    #[serial]
    async fn static_bearer_token_can_come_from_a_file() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::remove_var("KRAB_BEARER_TOKEN");
        std::env::set_var("KRAB_AUTH_MODE", "static");
        let path =
            std::env::temp_dir().join(format!("krab-bearer-{}-{}", std::process::id(), line!()));
        std::fs::write(&path, "file-token\n").unwrap();
        std::env::set_var("KRAB_BEARER_TOKEN_FILE", &path);

        let app = test_app();
        let request = |token: &str| {
            Request::builder()
                .uri("/protected")
                .header("Authorization", format!("Bearer {token}"))
                .header("x-forwarded-for", "10.10.0.43")
                .body(Body::empty())
                .unwrap()
        };
        let ok = app.clone().oneshot(request("file-token")).await.unwrap();
        let wrong = app.oneshot(request("other")).await.unwrap();

        std::env::remove_var("KRAB_BEARER_TOKEN_FILE");
        let _ = std::fs::remove_file(&path);
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    }

    // --- Remote JWKS, key retirement, failure reasons (0.6.0) -------------

    /// Throwaway Ed25519 test keys (generated for this suite, never used
    /// anywhere else) and the base64url `x` of each public key.
    const ED_KEY_1: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEID742l9H0wbtOWWM4nhgdHtQWwQJmJb39AreNwHcK+ee\n-----END PRIVATE KEY-----\n";
    const ED_X_1: &str = "tQCGXC6DpH3eQ7mQpTmUwz_UrjPnQ-X2ztczWt5Uyis";
    const ED_KEY_2: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIEB2k94KXBt5aiXPDWN5+lsLuPzYlsNw+PAD0dKDhH4x\n-----END PRIVATE KEY-----\n";
    const ED_X_2: &str = "x5VYN6QqYC4bfupaGI_894t7v2AfY3cu_GsHl8uV0dM";

    fn ed_token(kid: &str, pem: &str) -> String {
        let header = Header {
            kid: Some(kid.to_string()),
            alg: jsonwebtoken::Algorithm::EdDSA,
            ..Default::default()
        };
        encode(
            &header,
            &json!({"sub": "user", "exp": 9999999999i64}),
            &EncodingKey::from_ed_pem(pem.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn jwk(kid: &str, x: &str) -> Value {
        json!({"kty": "OKP", "crv": "Ed25519", "kid": kid, "x": x, "use": "sig", "alg": "EdDSA"})
    }

    /// A JWKS endpoint whose document can be swapped mid-test, counting hits.
    struct JwksServer {
        url: String,
        body: std::sync::Arc<std::sync::Mutex<Value>>,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    async fn jwks_server(initial: Value) -> JwksServer {
        let body = std::sync::Arc::new(std::sync::Mutex::new(initial));
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (b, h) = (body.clone(), hits.clone());
        let app = Router::new().route(
            "/jwks",
            axum::routing::get(move || {
                let (b, h) = (b.clone(), h.clone());
                async move {
                    h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let value = b.lock().unwrap().clone();
                    axum::Json(value)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        JwksServer {
            url: format!("http://{addr}/jwks"),
            body,
            hits,
        }
    }

    fn jwks_env(url: &str, min_refetch_secs: &str) {
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_ALLOWED_ALGS", "EdDSA");
        std::env::set_var("KRAB_OIDC_JWKS_URL", url);
        std::env::set_var("KRAB_OIDC_JWKS_MIN_REFETCH_SECS", min_refetch_secs);
    }

    fn clear_jwks_env() {
        for key in [
            "KRAB_OIDC_JWKS_URL",
            "KRAB_OIDC_JWKS_MIN_REFETCH_SECS",
            "KRAB_JWT_KEY_NOT_AFTER_JSON",
        ] {
            std::env::remove_var(key);
        }
    }

    async fn call(app: &Router, token: &str, ip: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("Authorization", format!("Bearer {token}"))
                    .header("x-forwarded-for", ip)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// A token signed by a key the provider publishes verifies, with no key
    /// material in the environment at all.
    #[tokio::test]
    #[serial]
    async fn jwks_keys_verify_tokens_without_static_key_material() {
        let _guard = env_lock();
        reset_auth_env();
        let server = jwks_server(json!({"keys": [jwk("k1", ED_X_1)]})).await;
        jwks_env(&server.url, "30");

        let app = test_app();
        assert_eq!(
            call(&app, &ed_token("k1", ED_KEY_1), "10.20.0.1").await,
            StatusCode::OK
        );
        // Signed by a key the provider does not publish under that kid.
        assert_eq!(
            call(&app, &ed_token("k1", ED_KEY_2), "10.20.0.2").await,
            StatusCode::UNAUTHORIZED
        );
        clear_jwks_env();
    }

    /// A provider-side rotation is picked up on the first token naming the
    /// new `kid`, without waiting for the scheduled refresh.
    #[tokio::test]
    #[serial]
    async fn jwks_refetches_on_an_unknown_kid() {
        let _guard = env_lock();
        reset_auth_env();
        let server = jwks_server(json!({"keys": [jwk("k1", ED_X_1)]})).await;
        // `0` is floored to one second: the refetch below must wait it out.
        jwks_env(&server.url, "0");

        let app = test_app();
        assert_eq!(
            call(&app, &ed_token("k1", ED_KEY_1), "10.20.0.3").await,
            StatusCode::OK
        );

        *server.body.lock().unwrap() = json!({"keys": [jwk("k1", ED_X_1), jwk("k2", ED_X_2)]});
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(
            call(&app, &ed_token("k2", ED_KEY_2), "10.20.0.4").await,
            StatusCode::OK
        );
        clear_jwks_env();
    }

    /// Unknown `kid`s cannot make the service hammer the identity provider:
    /// within the refetch window they are rejected from cache.
    #[tokio::test]
    #[serial]
    async fn jwks_unknown_kid_refetch_is_rate_limited() {
        let _guard = env_lock();
        reset_auth_env();
        let server = jwks_server(json!({"keys": [jwk("k1", ED_X_1)]})).await;
        jwks_env(&server.url, "3600");

        let (app, state) = test_app_and_state();
        state.runtime.jwt_verifier_cache.refresh_jwks().await;
        let before = server.hits.load(std::sync::atomic::Ordering::SeqCst);

        for i in 0..5 {
            let token = ed_token(&format!("made-up-{i}"), ED_KEY_2);
            assert_eq!(
                call(&app, &token, &format!("10.20.1.{i}")).await,
                StatusCode::UNAUTHORIZED
            );
        }
        let after = server.hits.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(after, before, "no refetch inside the rate-limit window");
        clear_jwks_env();
    }

    /// Requests that miss the cache for an unknown `kid` while a refetch is
    /// in flight share it: the need is re-checked after the fetch lock, so
    /// requests queued behind a (slow) fetch do not each fetch again in turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[serial]
    async fn jwks_refetch_is_single_flight() {
        let _guard = env_lock();
        reset_auth_env();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let slow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (counter, delay) = (hits.clone(), slow.clone());
        let server = Router::new().route(
            "/jwks",
            axum::routing::get(move || {
                let (counter, delay) = (counter.clone(), delay.clone());
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if delay.load(std::sync::atomic::Ordering::SeqCst) {
                        // Slower than the 1 s refetch gap.
                        tokio::time::sleep(Duration::from_millis(1300)).await;
                    }
                    axum::Json(json!({"keys": [jwk("k1", ED_X_1)]}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/jwks", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, server).await;
        });
        jwks_env(&url, "1");

        let (app, state) = test_app_and_state();
        state.runtime.jwt_verifier_cache.refresh_jwks().await;
        // Let the 1 s refetch gap pass, then make the IdP slow.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        slow.store(true, std::sync::atomic::Ordering::SeqCst);
        let before = hits.load(std::sync::atomic::Ordering::SeqCst);

        let token = ed_token("rotated", ED_KEY_2);
        let spawn_call = |i: usize| {
            let (app, token) = (app.clone(), token.clone());
            tokio::spawn(async move { call(&app, &token, &format!("10.20.2.{i}")).await })
        };
        // The first miss starts a slow fetch; the others arrive once the gap
        // since its start has passed, and queue behind it.
        let first = spawn_call(0);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let queued: Vec<_> = (1..4).map(spawn_call).collect();
        assert_eq!(first.await.unwrap(), StatusCode::UNAUTHORIZED);
        for request in queued {
            assert_eq!(request.await.unwrap(), StatusCode::UNAUTHORIZED);
        }
        let fetches = hits.load(std::sync::atomic::Ordering::SeqCst) - before;
        clear_jwks_env();
        assert_eq!(
            fetches, 1,
            "requests queued behind a fetch must not refetch"
        );
    }

    /// No key set has ever loaded: the outage is ours, so 503 — and the
    /// reason is recorded as provider_unavailable.
    #[tokio::test]
    #[serial]
    async fn jwks_unreachable_before_first_load_is_503() {
        let _guard = env_lock();
        reset_auth_env();
        // Bind and drop a listener so the port is closed.
        let url = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}/jwks", listener.local_addr().unwrap())
        };
        jwks_env(&url, "0");

        let (app, state) = test_app_and_state();
        assert_eq!(
            call(&app, &ed_token("k1", ED_KEY_1), "10.20.0.5").await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let slot = crate::http_auth::AuthFailureReason::ProviderUnavailable.slot();
        assert_eq!(
            state.runtime.auth_failure_reasons[slot].load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        clear_jwks_env();
    }

    /// Our outages are not the client's failures: a 503 while the key set is
    /// unreachable does not spend the client IP's auth-failure budget, so an
    /// IdP outage does not turn into 429s.
    #[tokio::test]
    #[serial]
    async fn provider_outage_does_not_count_against_the_client_ip() {
        let _guard = env_lock();
        reset_auth_env();
        let url = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            format!("http://{}/jwks", listener.local_addr().unwrap())
        };
        jwks_env(&url, "30");
        std::env::set_var("KRAB_AUTH_FAILURE_WINDOW_SECS", "3600");
        std::env::set_var("KRAB_AUTH_FAILURE_THRESHOLD", "1");

        let app = test_app();
        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(call(&app, &ed_token("k1", ED_KEY_1), "10.20.3.1").await);
        }
        let bad_token = call(&app, "garbage", "10.20.3.1").await;
        std::env::remove_var("KRAB_AUTH_FAILURE_WINDOW_SECS");
        std::env::remove_var("KRAB_AUTH_FAILURE_THRESHOLD");
        clear_jwks_env();

        assert_eq!(statuses, vec![StatusCode::SERVICE_UNAVAILABLE; 3]);
        assert_eq!(
            bad_token,
            StatusCode::UNAUTHORIZED,
            "the outage must not have used up the client's failure budget"
        );
    }

    /// A plain-http JWKS URL outside dev is refused at construction: nothing
    /// verifies (503) rather than trusting keys anyone on the path could swap.
    #[tokio::test]
    #[serial]
    async fn jwks_plain_http_url_is_refused_outside_dev() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        jwks_env("http://idp.example.com/jwks", "30");

        let (app, state) = test_app_and_state();
        assert!(state.runtime.jwt_verifier_cache.load_failed());
        assert_eq!(
            call(&app, &ed_token("k1", ED_KEY_1), "10.20.0.6").await,
            StatusCode::SERVICE_UNAVAILABLE
        );
        clear_jwks_env();
    }

    /// A key past its `key_not_after` stops verifying; its successor keeps
    /// working. The failure is counted as key_retired.
    #[tokio::test]
    #[serial]
    async fn a_retired_key_stops_verifying_on_schedule() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_KEYS_JSON", r#"{"old": "s-old", "new": "s-new"}"#);
        std::env::set_var(
            "KRAB_JWT_KEY_NOT_AFTER_JSON",
            r#"{"old": "2000-01-01T00:00:00Z", "new": "2999-01-01T00:00:00Z"}"#,
        );

        let (app, state) = test_app_and_state();
        let claims = json!({"sub": "user", "exp": 9999999999i64});
        let old = generate_token_with_kid("old", claims.clone(), b"s-old");
        let new = generate_token_with_kid("new", claims, b"s-new");

        assert_eq!(
            call(&app, &old, "10.20.0.7").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(call(&app, &new, "10.20.0.8").await, StatusCode::OK);
        let slot = crate::http_auth::AuthFailureReason::KeyRetired.slot();
        assert_eq!(
            state.runtime.auth_failure_reasons[slot].load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        clear_jwks_env();
    }

    /// An unparseable retirement time fails closed instead of meaning "never".
    #[tokio::test]
    #[serial]
    async fn an_unparseable_key_not_after_fails_closed() {
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_KEYS_JSON", r#"{"old": "s-old"}"#);
        std::env::set_var("KRAB_JWT_KEY_NOT_AFTER_JSON", r#"{"old": "next tuesday"}"#);

        let (_app, state) = test_app_and_state();
        assert!(state.runtime.jwt_verifier_cache.load_failed());
        clear_jwks_env();
    }

    /// Each failure is counted under its reason, and the exposition carries
    /// the labelled series.
    #[tokio::test]
    #[serial]
    async fn auth_failures_are_counted_by_reason() {
        use crate::http_auth::AuthFailureReason as R;
        let _guard = env_lock();
        reset_auth_env();
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_JWT_SECRET", "secret");
        std::env::set_var("KRAB_AUTH_FAILURE_THRESHOLD", "1000");

        let (app, state) = test_app_and_state();
        let expired = generate_token(json!({"sub": "u", "exp": 1000000000}));
        let wrong_key = encode(
            &Header::default(),
            &json!({"sub": "u", "exp": 9999999999i64}),
            &EncodingKey::from_secret(b"not-the-secret"),
        )
        .unwrap();

        let missing = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/protected")
                    .header("x-forwarded-for", "10.20.0.9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        call(&app, "garbage", "10.20.0.9").await;
        call(&app, &expired, "10.20.0.9").await;
        call(&app, &wrong_key, "10.20.0.9").await;

        let count = |r: R| {
            state.runtime.auth_failure_reasons[r.slot()].load(std::sync::atomic::Ordering::Relaxed)
        };
        assert_eq!(count(R::MissingCredentials), 1);
        assert_eq!(count(R::MalformedToken), 1);
        assert_eq!(count(R::Expired), 1);
        assert_eq!(count(R::InvalidSignature), 1);

        let body = crate::http_runtime::metrics_prometheus_impl(&state.runtime);
        let bytes = axum::body::to_bytes(body.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            text.contains("krab_auth_failures_by_reason_total{reason=\"expired\"} 1"),
            "{text}"
        );
        std::env::remove_var("KRAB_AUTH_FAILURE_THRESHOLD");
    }
}
