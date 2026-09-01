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
            "KRAB_METRICS_PUBLIC",
            "KRAB_TRUST_PROXY_HEADERS",
            "KRAB_TRUSTED_PROXY_HOPS",
            "KRAB_RATE_LIMIT_CAPACITY",
            "KRAB_RATE_LIMIT_REFILL_PER_SEC",
            "KRAB_RATE_LIMIT_FAIL_OPEN",
            "KRAB_HTTP_REQUEST_TIMEOUT_SECS",
            "KRAB_HTTP_MAX_CONCURRENCY",
            "KRAB_HTTP_OVERLOAD_MODE",
            "KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED",
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
}
