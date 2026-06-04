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
fn split_library_exports_build_app_and_transport_modules() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let lib_source = manifest_dir.join("src").join("lib.rs");
    let raw = std::fs::read_to_string(&lib_source)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", lib_source.display(), e));

    assert!(
        raw.contains("pub fn build_app"),
        "split library must export build_app"
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

#[tokio::test]
async fn split_runtime_rest_and_graphql_me_are_parity_aligned() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use krab_core::http::AuthContext;
    use serde_json::Value;
    use tower::ServiceExt;

    let app = service_users_split::build_app(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    );

    let rest = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/users/me")
                .extension(AuthContext {
                    subject: Some("user-1".to_string()),
                    issuer: Some("tests".to_string()),
                    provider: Some("tests".to_string()),
                    token_id: None,
                    token_use: None,
                    scopes: vec![],
                    tenant_id: Some("tenant-a".to_string()),
                    roles: vec![],
                })
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rest.status(), StatusCode::OK);
    let rest_body = rest.into_body().collect().await.unwrap().to_bytes();
    let rest_json: Value = serde_json::from_slice(&rest_body).unwrap();

    let graphql = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/graphql")
                .header("content-type", "application/json")
                .extension(AuthContext {
                    subject: Some("user-1".to_string()),
                    issuer: Some("tests".to_string()),
                    provider: Some("tests".to_string()),
                    token_id: None,
                    token_use: None,
                    scopes: vec![],
                    tenant_id: Some("tenant-a".to_string()),
                    roles: vec![],
                })
                .body(Body::from(r#"{"query":"{ me { id username } }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(graphql.status(), StatusCode::OK);
    let graphql_body = graphql.into_body().collect().await.unwrap().to_bytes();
    let graphql_json: Value = serde_json::from_slice(&graphql_body).unwrap();

    assert_eq!(
        rest_json.get("id"),
        graphql_json
            .get("data")
            .and_then(|d| d.get("me"))
            .and_then(|m| m.get("id"))
    );
    assert_eq!(
        rest_json.get("username"),
        graphql_json
            .get("data")
            .and_then(|d| d.get("me"))
            .and_then(|m| m.get("username"))
    );
}

#[tokio::test]
async fn split_runtime_missing_auth_is_rejected() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let app = service_users_split::build_app(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    );

    let rest = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/users/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rest.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let graphql = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/graphql")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"{ me { id username } }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(graphql.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn split_runtime_missing_tenant_is_rejected() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use krab_core::http::AuthContext;
    use serde_json::Value;
    use tower::ServiceExt;

    let app = service_users_split::build_app(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    );

    let rest = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/users/me")
                .extension(AuthContext {
                    subject: Some("user-1".to_string()),
                    issuer: Some("tests".to_string()),
                    provider: Some("tests".to_string()),
                    token_id: None,
                    token_use: None,
                    scopes: vec![],
                    tenant_id: None,
                    roles: vec![],
                })
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rest.status(), StatusCode::BAD_REQUEST);

    let graphql = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/graphql")
                .header("content-type", "application/json")
                .extension(AuthContext {
                    subject: Some("user-1".to_string()),
                    issuer: Some("tests".to_string()),
                    provider: Some("tests".to_string()),
                    token_id: None,
                    token_use: None,
                    scopes: vec![],
                    tenant_id: None,
                    roles: vec![],
                })
                .body(Body::from(r#"{"query":"{ me { id username } }"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
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

#[tokio::test]
async fn split_runtime_unsupported_rpc_transport_stays_absent() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let app = service_users_split::build_app(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    );
    let rpc = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/rpc")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"method":"users.getMe","params":{},"id":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(rpc.status(), StatusCode::NOT_FOUND);
}

fn normalize_schema(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
