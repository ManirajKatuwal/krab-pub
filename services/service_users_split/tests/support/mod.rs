//! Shared fixtures for the runtime HTTP suites.
//!
//! Everything here builds the service the way `run_default` does and talks to
//! it the way a client does. Nothing in this module inserts a request
//! extension: the governance layers are what must produce `AuthContext`, and a
//! test that supplies one itself cannot notice when they stop being applied.

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// HMAC secret the test provider signs and verifies with. Dev-only value; the
/// service refuses inline secrets outside dev.
const TEST_JWT_SECRET: &str = "krab-users-split-test-secret";

/// Variables the auth, protocol, and runtime layers read. The guard pins every
/// one of them so a suite is not steered by the developer's shell or by a
/// sibling test, and restores the previous values on drop.
const MANAGED_ENV_VARS: &[&str] = &[
    "KRAB_AUTH_ADMIN_ROLE",
    "KRAB_AUTH_ADMIN_SCOPE",
    "KRAB_AUTH_MODE",
    "KRAB_AUTH_OPEN_PATHS",
    "KRAB_AUTH_PUBLIC_PATHS",
    "KRAB_AUTH_REQUIRED_ROLES",
    "KRAB_AUTH_REQUIRED_SCOPES",
    "KRAB_AUTH_REQUIRE_TENANT_CLAIM",
    "KRAB_AUTH_ROUTE_POLICIES_JSON",
    "KRAB_BEARER_TOKEN",
    "KRAB_CSRF_ENABLED",
    "KRAB_ENVIRONMENT",
    "KRAB_JWT_ALLOWED_ALGS",
    "KRAB_JWT_KEYS_JSON",
    "KRAB_JWT_PROVIDERS_JSON",
    "KRAB_JWT_REQUIRE_KID",
    "KRAB_JWT_SECRET",
    "KRAB_METRICS_PUBLIC",
    "KRAB_OIDC_AUDIENCE",
    "KRAB_OIDC_ISSUER",
    "KRAB_PROTOCOL_DEFAULT",
    "KRAB_PROTOCOL_ENABLED",
    "KRAB_PROTOCOL_ENABLED_USERS_SPLIT",
    "KRAB_PROTOCOL_EXPOSURE_MODE",
    "KRAB_PROTOCOL_TOPOLOGY",
    "KRAB_REDIS_URL",
    "KRAB_SERVICE_NAME",
];

pub struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Pin the environment to dev-mode JWT auth with a known signing secret.
///
/// Suites using this must be `#[serial]` — the process environment is shared.
pub fn auth_env_guard() -> EnvGuard {
    let previous = MANAGED_ENV_VARS
        .iter()
        .map(|key| (*key, std::env::var(key).ok()))
        .collect();

    for key in MANAGED_ENV_VARS {
        std::env::remove_var(key);
    }
    std::env::set_var("KRAB_ENVIRONMENT", "dev");
    std::env::set_var("KRAB_AUTH_MODE", "jwt");
    std::env::set_var("KRAB_JWT_SECRET", TEST_JWT_SECRET);

    EnvGuard { previous }
}

/// The application `run_default` serves, minus the listener.
pub fn spawn_app() -> Router {
    service_users_split::build_default_app(
        service_users_split::domain::service::InMemoryDomainService::shared(),
    )
    .expect("split service app should build")
}

#[derive(Serialize)]
struct TestClaims {
    sub: String,
    exp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tid: Option<String>,
}

/// Mint the bearer token a client would present: HS256, signed with the same
/// secret the service verifies against, carrying `tid` as the tenant claim.
pub fn bearer_token(tenant_id: Option<&str>) -> String {
    let exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after the unix epoch")
        .as_secs() as i64
        + 300;

    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &TestClaims {
            sub: "user-1".to_string(),
            exp,
            tid: tenant_id.map(ToString::to_string),
        },
        &jsonwebtoken::EncodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
    )
    .expect("test token should encode")
}

fn with_optional_bearer(
    builder: axum::http::request::Builder,
    token: Option<&str>,
) -> axum::http::request::Builder {
    match token {
        Some(token) => builder.header("authorization", format!("Bearer {token}")),
        None => builder,
    }
}

pub fn rest_me_request(token: Option<&str>) -> Request<Body> {
    with_optional_bearer(
        Request::builder().method("GET").uri("/api/v1/users/me"),
        token,
    )
    .body(Body::empty())
    .expect("rest request should build")
}

pub fn graphql_request(token: Option<&str>) -> Request<Body> {
    with_optional_bearer(
        Request::builder()
            .method("POST")
            .uri("/api/v1/graphql")
            .header("content-type", "application/json"),
        token,
    )
    .body(Body::from(r#"{"query":"{ me { id username } }"}"#))
    .expect("graphql request should build")
}
