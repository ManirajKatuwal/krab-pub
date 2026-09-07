//! Contract and runtime conformance for the split-topology reference service.
//!
//! The HTTP suites here drive `build_default_app` — the same router
//! `run_default` serves, governance layers included — and authenticate the way
//! a client does, with a bearer token. They deliberately do **not** attach an
//! `AuthContext` extension to the request: an earlier version of this file did,
//! which is why the service could ship with `build_app` never applying
//! `apply_common_http_layers` and every `/api/v1/*` route answering 500 to real
//! traffic while the suite stayed green. No HTTP client can inject an
//! extension, so no test here may either.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use serial_test::serial;
use support::{auth_env_guard, bearer_token, graphql_request, rest_me_request, spawn_app};
use tower::ServiceExt;

#[test]
fn contract_fixtures_and_docs_exist_for_users_split() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let users_service_contract = manifest_dir
        .join("..")
        .join("service_users")
        .join("contracts")
        .join("users_gateway_upstreams_v1.json");
    let users_split_readme = manifest_dir.join("README.md");

    assert!(
        users_service_contract.exists(),
        "missing upstream contract fixture: {}",
        users_service_contract.display()
    );
    assert!(
        users_split_readme.exists(),
        "missing users_split documentation: {}",
        users_split_readme.display()
    );
}

#[test]
fn split_rest_adapter_source_exports_users_me_route() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let rest_source = manifest_dir.join("src").join("adapters").join("rest.rs");
    let raw = std::fs::read_to_string(&rest_source)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", rest_source.display(), e));

    assert!(
        raw.contains("/users/me"),
        "split REST adapter must expose /users/me route"
    );
    assert!(
        raw.contains("tenant context required"),
        "split REST adapter must enforce tenant context"
    );
}

#[test]
fn split_graphql_adapter_source_exports_me_field() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let graphql_source = manifest_dir.join("src").join("adapters").join("graphql.rs");
    let raw = std::fs::read_to_string(&graphql_source)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", graphql_source.display(), e));

    assert!(
        raw.contains("async fn me"),
        "split GraphQL adapter must expose me field"
    );
    assert!(
        raw.contains("tenant context is required"),
        "split GraphQL adapter must enforce tenant context"
    );
}

#[test]
fn split_library_exports_app_builders_and_transport_modules() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib_source = manifest_dir.join("src").join("lib.rs");
    let raw = std::fs::read_to_string(&lib_source)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", lib_source.display(), e));

    assert!(
        raw.contains("pub use crate::runtime::{build_app, AppState}"),
        "split library must re-export build_app and AppState"
    );
    assert!(
        raw.contains("pub fn build_default_app"),
        "split library must export build_default_app"
    );
    assert!(
        raw.contains("pub mod adapters"),
        "split library must export adapters module"
    );
    assert!(
        raw.contains("pub mod domain"),
        "split library must export domain module"
    );
}

/// The runtime seam that must never regress: the governance layers are what
/// insert `AuthContext`, so an unauthenticated API call is a 401, never the
/// 500 a missing extension produces.
#[tokio::test]
#[serial]
async fn split_runtime_unauthenticated_api_routes_are_rejected_not_500() {
    let _env = auth_env_guard();
    let app = spawn_app();

    for (label, request) in [
        ("rest", rest_me_request(None)),
        ("graphql", graphql_request(None)),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{label} route must reject an unauthenticated caller with 401"
        );
    }
}

/// Liveness and readiness stay anonymous — the orchestrator health-checks
/// `/ready` and must not need a token.
#[tokio::test]
#[serial]
async fn split_runtime_operational_endpoints_stay_open() {
    let _env = auth_env_guard();
    let app = spawn_app();

    for path in ["/health", "/ready"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path} must stay open");
    }
}

/// Metrics are closed by default, so an anonymous scrape is rejected.
#[tokio::test]
#[serial]
async fn split_runtime_metrics_are_closed_by_default() {
    let _env = auth_env_guard();
    let app = spawn_app();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial]
async fn split_runtime_rest_and_graphql_me_are_parity_aligned() {
    let _env = auth_env_guard();
    let app = spawn_app();
    let token = bearer_token(Some("tenant-a"));

    let rest = app
        .clone()
        .oneshot(rest_me_request(Some(&token)))
        .await
        .unwrap();
    assert_eq!(rest.status(), StatusCode::OK);
    let rest_body = rest.into_body().collect().await.unwrap().to_bytes();
    let rest_json: Value = serde_json::from_slice(&rest_body).unwrap();

    let graphql = app.oneshot(graphql_request(Some(&token))).await.unwrap();
    assert_eq!(graphql.status(), StatusCode::OK);
    let graphql_body = graphql.into_body().collect().await.unwrap().to_bytes();
    let graphql_json: Value = serde_json::from_slice(&graphql_body).unwrap();

    let graphql_me = graphql_json.get("data").and_then(|d| d.get("me"));
    assert_eq!(rest_json.get("id"), graphql_me.and_then(|m| m.get("id")));
    assert_eq!(
        rest_json.get("username"),
        graphql_me.and_then(|m| m.get("username"))
    );
    assert_eq!(
        rest_json.get("username").and_then(Value::as_str),
        Some("krab_user_tenant-a"),
        "the tenant must come from the verified token, not a request header"
    );
}

#[tokio::test]
#[serial]
async fn split_runtime_token_without_tenant_claim_is_rejected() {
    let _env = auth_env_guard();
    let app = spawn_app();
    let token = bearer_token(None);

    let rest = app
        .clone()
        .oneshot(rest_me_request(Some(&token)))
        .await
        .unwrap();
    assert_eq!(rest.status(), StatusCode::BAD_REQUEST);

    let graphql = app.oneshot(graphql_request(Some(&token))).await.unwrap();
    assert_eq!(graphql.status(), StatusCode::OK);
    let graphql_body = graphql.into_body().collect().await.unwrap().to_bytes();
    let graphql_json: Value = serde_json::from_slice(&graphql_body).unwrap();
    let first_error = graphql_json
        .get("errors")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str());
    assert_eq!(first_error, Some("tenant context is required"));
}

/// The service-local protocol override has to work under the name this service
/// advertises, with or without `KRAB_SERVICE_NAME` in the environment.
///
/// `ProtocolConfig::from_env` derives `KRAB_PROTOCOL_ENABLED_<NAME>` from
/// `KRAB_SERVICE_NAME`/`KRAB_SERVICE`, falling back to the literal `service` —
/// so `cargo run --bin service_users_split`, which the README documents and
/// which sets neither, made `KRAB_PROTOCOL_ENABLED_USERS_SPLIT` inert. Asking
/// for GraphQL got REST-only and a `PROTOCOL_NOT_SUPPORTED` on the route.
#[tokio::test]
#[serial]
async fn split_runtime_service_local_protocol_override_applies_without_krab_service_name() {
    let _env = auth_env_guard();
    std::env::set_var("KRAB_PROTOCOL_EXPOSURE_MODE", "multi");
    std::env::set_var("KRAB_PROTOCOL_ENABLED_USERS_SPLIT", "rest,graphql");

    let app = spawn_app();
    let token = bearer_token(Some("tenant-a"));

    let graphql = app
        .clone()
        .oneshot(graphql_request(Some(&token)))
        .await
        .unwrap();
    assert_eq!(
        graphql.status(),
        StatusCode::OK,
        "KRAB_PROTOCOL_ENABLED_USERS_SPLIT must enable the GraphQL adapter"
    );

    let rest = app.oneshot(rest_me_request(Some(&token))).await.unwrap();
    assert_eq!(rest.status(), StatusCode::OK);
}

/// The same override, with the environment also naming the service — the shape
/// `krab.toml` produces. Both paths must land on the same protocol set.
#[tokio::test]
#[serial]
async fn split_runtime_service_local_protocol_override_applies_with_krab_service_name() {
    let _env = auth_env_guard();
    std::env::set_var("KRAB_SERVICE_NAME", "users-split");
    std::env::set_var("KRAB_PROTOCOL_EXPOSURE_MODE", "multi");
    std::env::set_var("KRAB_PROTOCOL_ENABLED_USERS_SPLIT", "rest,graphql");

    let app = spawn_app();
    let token = bearer_token(Some("tenant-a"));

    let graphql = app
        .clone()
        .oneshot(graphql_request(Some(&token)))
        .await
        .unwrap();
    assert_eq!(graphql.status(), StatusCode::OK);

    let rest = app.oneshot(rest_me_request(Some(&token))).await.unwrap();
    assert_eq!(rest.status(), StatusCode::OK);
}

/// No RPC adapter is mounted, and protocol resolution rejects the route family
/// before routing can 404 it.
#[tokio::test]
#[serial]
async fn split_runtime_unsupported_rpc_transport_stays_absent() {
    let _env = auth_env_guard();
    let app = spawn_app();
    let token = bearer_token(Some("tenant-a"));

    let rpc = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/rpc")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"method":"users.getMe","params":{},"id":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(rpc.status(), StatusCode::BAD_REQUEST);
    let body = rpc.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json.get("code").and_then(Value::as_str),
        Some("PROTOCOL_NOT_SUPPORTED")
    );
}

#[test]
fn split_graphql_schema_matches_baseline_snapshot() {
    let schema = service_users_split::adapters::graphql::build_schema(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    )
    .sdl();
    let baseline = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("service_users")
            .join("contracts")
            .join("graphql_schema_v1.graphql"),
    )
    .unwrap();

    assert_eq!(normalize_schema(&schema), normalize_schema(&baseline));
}

fn normalize_schema(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
