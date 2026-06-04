use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};

use crate::http::{constant_time_eq, current_window_epoch, is_admin_api_path, parse_csv_set};
use crate::http_runtime::HasRuntimeState;
use crate::http_security::extract_client_ip;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub subject: Option<String>,
    pub issuer: Option<String>,
    pub provider: Option<String>,
    pub token_id: Option<String>,
    pub token_use: Option<String>,
    pub tenant_id: Option<String>,
    pub scopes: Vec<String>,
    pub roles: Vec<String>,
}

pub fn has_admin_entitlement(ctx: &AuthContext) -> bool {
    let admin_scope =
        std::env::var("KRAB_AUTH_ADMIN_SCOPE").unwrap_or_else(|_| "admin".to_string());
    let admin_role = std::env::var("KRAB_AUTH_ADMIN_ROLE").unwrap_or_else(|_| "admin".to_string());
    ctx.scopes.iter().any(|s| s == &admin_scope) || ctx.roles.iter().any(|r| r == &admin_role)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JwtClaims {
    pub sub: Option<String>,
    pub iss: Option<String>,
    pub aud: Option<Value>,
    pub exp: Option<i64>,
    pub jti: Option<String>,
    pub token_use: Option<String>,
    pub tid: Option<String>,
    pub tenant_id: Option<String>,
    pub scope: Option<String>,
    pub scp: Option<Value>,
    pub roles: Option<Vec<String>>,
    pub role: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwtProviderConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub audience: Option<String>,
    pub keys: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub required_claims: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutePolicy {
    pub prefix: String,
    #[serde(default)]
    pub all_scopes: Vec<String>,
    #[serde(default)]
    pub any_scopes: Vec<String>,
    #[serde(default)]
    pub all_roles: Vec<String>,
    #[serde(default)]
    pub any_roles: Vec<String>,
    #[serde(default)]
    pub allow_subjects: Vec<String>,
    #[serde(default)]
    pub require_tenant_match: bool,
}

fn parse_jwt_algorithm(alg: &str) -> Option<jsonwebtoken::Algorithm> {
    match alg.trim().to_ascii_uppercase().as_str() {
        "HS256" => Some(jsonwebtoken::Algorithm::HS256),
        "HS384" => Some(jsonwebtoken::Algorithm::HS384),
        "HS512" => Some(jsonwebtoken::Algorithm::HS512),
        "RS256" => Some(jsonwebtoken::Algorithm::RS256),
        "RS384" => Some(jsonwebtoken::Algorithm::RS384),
        "RS512" => Some(jsonwebtoken::Algorithm::RS512),
        "ES256" => Some(jsonwebtoken::Algorithm::ES256),
        "ES384" => Some(jsonwebtoken::Algorithm::ES384),
        "PS256" => Some(jsonwebtoken::Algorithm::PS256),
        "PS384" => Some(jsonwebtoken::Algorithm::PS384),
        "PS512" => Some(jsonwebtoken::Algorithm::PS512),
        "EDDSA" => Some(jsonwebtoken::Algorithm::EdDSA),
        _ => None,
    }
}

fn configured_jwt_algorithms() -> Result<Vec<jsonwebtoken::Algorithm>, StatusCode> {
    let raw = match std::env::var("KRAB_JWT_ALLOWED_ALGS") {
        Ok(raw) => raw,
        Err(_) => return Ok(vec![jsonwebtoken::Algorithm::HS256]),
    };

    let mut algorithms = Vec::new();
    for alg in raw.split(',').map(str::trim).filter(|alg| !alg.is_empty()) {
        let Some(parsed) = parse_jwt_algorithm(alg) else {
            warn!(alg = %alg, "jwt_allowlist_contains_unsupported_algorithm");
            continue;
        };

        if !algorithms.contains(&parsed) {
            algorithms.push(parsed);
        }
    }

    if algorithms.is_empty() {
        warn!("KRAB_JWT_ALLOWED_ALGS resolved to an empty allowlist");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(algorithms)
}

pub fn jwt_algorithm_allowed(alg: &str, _environment: &crate::config::Environment) -> bool {
    let Some(candidate) = parse_jwt_algorithm(alg) else {
        return false;
    };

    configured_jwt_algorithms()
        .map(|allowed| allowed.contains(&candidate))
        .unwrap_or(false)
}

pub fn jwt_leeway_secs() -> u64 {
    std::env::var("KRAB_JWT_LEEWAY_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
}

pub fn load_rotation_keys() -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    if let Some(json) = crate::config::read_env_or_file("KRAB_JWT_KEYS_JSON")? {
        let parsed = serde_json::from_str::<std::collections::BTreeMap<String, String>>(&json)?;
        if !parsed.is_empty() {
            return Ok(parsed);
        }
    }

    let secret = crate::config::read_env_or_file("KRAB_JWT_SECRET")?;

    let Some(secret) = secret else {
        tracing::warn!(
            "No JWT signing key configured; set KRAB_JWT_SECRET or KRAB_JWT_KEYS_JSON for jwt/oidc mode"
        );
        return Ok(std::collections::BTreeMap::new());
    };

    let mut keys = std::collections::BTreeMap::new();
    keys.insert("default".to_string(), secret);
    Ok(keys)
}

pub fn load_jwt_providers() -> anyhow::Result<Vec<JwtProviderConfig>> {
    if let Some(raw) = crate::config::read_env_or_file("KRAB_JWT_PROVIDERS_JSON")? {
        let mut providers = serde_json::from_str::<Vec<JwtProviderConfig>>(&raw)?;
        providers.retain(|p| !p.keys.is_empty());
        if !providers.is_empty() {
            return Ok(providers);
        }
    }

    Ok(vec![JwtProviderConfig {
        name: Some("default".to_string()),
        issuer: std::env::var("KRAB_OIDC_ISSUER").ok(),
        audience: std::env::var("KRAB_OIDC_AUDIENCE").ok(),
        keys: load_rotation_keys()?,
        required_claims: std::collections::BTreeMap::new(),
    }])
}

pub fn require_kid() -> bool {
    std::env::var("KRAB_JWT_REQUIRE_KID")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn select_key<'a>(
    keys: &'a std::collections::BTreeMap<String, String>,
    kid: Option<&str>,
) -> Option<&'a String> {
    match kid {
        Some(k) => keys.get(k),
        None if require_kid() => None,
        None => keys.get("default").or_else(|| keys.values().next()),
    }
}

pub fn tenant_from_claims(claims: &JwtClaims) -> Option<String> {
    claims
        .tenant_id
        .clone()
        .or_else(|| claims.tid.clone())
        .or_else(|| {
            claims
                .extra
                .get("tenant_id")
                .and_then(|v| v.as_str().map(ToString::to_string))
        })
        .or_else(|| {
            claims
                .extra
                .get("tid")
                .and_then(|v| v.as_str().map(ToString::to_string))
        })
}

pub fn tenant_from_path(path: &str) -> Option<&str> {
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    while let Some(segment) = parts.next() {
        if segment == "tenants" {
            return parts.next();
        }
    }
    None
}

pub fn load_route_policies() -> Vec<RoutePolicy> {
    std::env::var("KRAB_AUTH_ROUTE_POLICIES_JSON")
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<RoutePolicy>>(&raw).ok())
        .unwrap_or_default()
}

pub fn validate_provider_claims(
    claims: &JwtClaims,
    provider: &JwtProviderConfig,
) -> Result<(), StatusCode> {
    if let Some(expected_issuer) = provider.issuer.as_deref() {
        if claims.iss.as_deref() != Some(expected_issuer) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    if let Some(expected_audience) = provider.audience.as_ref() {
        let aud_ok = match claims.aud.as_ref() {
            Some(Value::String(aud)) => aud == expected_audience,
            Some(Value::Array(values)) => values
                .iter()
                .any(|v| v.as_str() == Some(expected_audience.as_str())),
            _ => false,
        };
        if !aud_ok {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    if !provider.required_claims.is_empty() {
        let claims_json = serde_json::to_value(claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let claims_obj = claims_json.as_object().ok_or(StatusCode::UNAUTHORIZED)?;
        for (k, v) in &provider.required_claims {
            let found = claims_obj
                .get(k)
                .or_else(|| claims.extra.get(k))
                .ok_or(StatusCode::UNAUTHORIZED)?;
            if found != v {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }

    Ok(())
}

pub fn scopes_from_claims(claims: &JwtClaims) -> Vec<String> {
    if let Some(scope) = &claims.scope {
        return scope.split_whitespace().map(|s| s.to_string()).collect();
    }

    match claims.scp.as_ref() {
        Some(Value::String(scope)) => scope.split_whitespace().map(|s| s.to_string()).collect(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    }
}

pub fn roles_from_claims(claims: &JwtClaims) -> Vec<String> {
    if let Some(roles) = &claims.roles {
        return roles.clone();
    }
    if let Some(role) = &claims.role {
        return vec![role.clone()];
    }
    vec![]
}

fn decoding_key_for_algorithm(
    algorithm: jsonwebtoken::Algorithm,
    key_material: &str,
) -> anyhow::Result<jsonwebtoken::DecodingKey> {
    match algorithm {
        jsonwebtoken::Algorithm::HS256
        | jsonwebtoken::Algorithm::HS384
        | jsonwebtoken::Algorithm::HS512 => Ok(jsonwebtoken::DecodingKey::from_secret(
            key_material.as_bytes(),
        )),
        jsonwebtoken::Algorithm::RS256
        | jsonwebtoken::Algorithm::RS384
        | jsonwebtoken::Algorithm::RS512
        | jsonwebtoken::Algorithm::PS256
        | jsonwebtoken::Algorithm::PS384
        | jsonwebtoken::Algorithm::PS512 => Ok(jsonwebtoken::DecodingKey::from_rsa_pem(
            key_material.as_bytes(),
        )?),
        jsonwebtoken::Algorithm::ES256 | jsonwebtoken::Algorithm::ES384 => Ok(
            jsonwebtoken::DecodingKey::from_ec_pem(key_material.as_bytes())?,
        ),
        jsonwebtoken::Algorithm::EdDSA => Ok(jsonwebtoken::DecodingKey::from_ed_pem(
            key_material.as_bytes(),
        )?),
    }
}

pub fn enforce_claim_policy(
    path: &str,
    claims: &JwtClaims,
    tenant_id: Option<&str>,
    scopes: &[String],
    roles: &[String],
) -> Result<(), StatusCode> {
    let required_scopes = std::env::var("KRAB_AUTH_REQUIRED_SCOPES")
        .ok()
        .map(|v| parse_csv_set(&v))
        .unwrap_or_default();
    for scope in required_scopes {
        if !scopes.iter().any(|s| s == &scope) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let required_roles = std::env::var("KRAB_AUTH_REQUIRED_ROLES")
        .ok()
        .map(|v| parse_csv_set(&v))
        .unwrap_or_default();
    for role in required_roles {
        if !roles.iter().any(|r| r == &role) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    if is_admin_api_path(path) {
        let admin_scope =
            std::env::var("KRAB_AUTH_ADMIN_SCOPE").unwrap_or_else(|_| "admin".to_string());
        let admin_role =
            std::env::var("KRAB_AUTH_ADMIN_ROLE").unwrap_or_else(|_| "admin".to_string());
        if !scopes.iter().any(|s| s == &admin_scope) && !roles.iter().any(|r| r == &admin_role) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    if crate::http::bool_env("KRAB_AUTH_REQUIRE_TENANT_CLAIM", false) && tenant_id.is_none() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    if crate::http::bool_env("KRAB_AUTH_REQUIRE_TENANT_MATCH", true) {
        if let Some(path_tenant) = tenant_from_path(path) {
            if tenant_id != Some(path_tenant) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }

    for policy in load_route_policies()
        .into_iter()
        .filter(|p| path.starts_with(&p.prefix))
    {
        for scope in &policy.all_scopes {
            if !scopes.iter().any(|s| s == scope) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        if !policy.any_scopes.is_empty()
            && !policy
                .any_scopes
                .iter()
                .any(|s| scopes.iter().any(|actual| actual == s))
        {
            return Err(StatusCode::UNAUTHORIZED);
        }

        for role in &policy.all_roles {
            if !roles.iter().any(|r| r == role) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
        if !policy.any_roles.is_empty()
            && !policy
                .any_roles
                .iter()
                .any(|r| roles.iter().any(|actual| actual == r))
        {
            return Err(StatusCode::UNAUTHORIZED);
        }

        if !policy.allow_subjects.is_empty() {
            let subject = claims.sub.as_deref().ok_or(StatusCode::UNAUTHORIZED)?;
            if !policy.allow_subjects.iter().any(|s| s == subject) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }

        if policy.require_tenant_match {
            let path_tenant = tenant_from_path(path).ok_or(StatusCode::UNAUTHORIZED)?;
            if tenant_id != Some(path_tenant) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }

    if let Ok(required_claims) = std::env::var("KRAB_AUTH_REQUIRED_CLAIMS_JSON") {
        let required: std::collections::BTreeMap<String, Value> =
            serde_json::from_str(&required_claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let claims_json = serde_json::to_value(claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let claims_obj = claims_json.as_object().ok_or(StatusCode::UNAUTHORIZED)?;
        for (k, v) in required {
            let found = claims_obj
                .get(&k)
                .or_else(|| claims.extra.get(&k))
                .ok_or(StatusCode::UNAUTHORIZED)?;
            if found != &v {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }
    }

    Ok(())
}

pub fn authorize_with_static_bearer(req: &Request<Body>) -> Result<AuthContext, StatusCode> {
    let expected = std::env::var("KRAB_BEARER_TOKEN").map_err(|_| {
        tracing::warn!("KRAB_BEARER_TOKEN is not configured for static auth mode");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    if expected.trim().is_empty() {
        tracing::warn!("KRAB_BEARER_TOKEN is empty and cannot be used for static auth mode");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let expected = format!("Bearer {expected}");

    let authorized = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .map(|v| constant_time_eq(v.as_bytes(), expected.as_bytes()))
        .unwrap_or(false);

    if !authorized {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(AuthContext {
        subject: Some("static-token-client".to_string()),
        issuer: Some("krab.static".to_string()),
        provider: Some("static".to_string()),
        token_id: None,
        token_use: None,
        tenant_id: None,
        scopes: vec![],
        roles: vec![],
    })
}

pub fn authorize_with_jwt(req: &Request<Body>, path: &str) -> Result<AuthContext, StatusCode> {
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let header = jsonwebtoken::decode_header(bearer).map_err(|_| StatusCode::UNAUTHORIZED)?;

    let env = crate::config::Environment::from_env();
    let alg = format!("{:?}", header.alg);
    let allowed_algs = configured_jwt_algorithms()?;
    if !allowed_algs.contains(&header.alg) || !jwt_algorithm_allowed(&alg, &env) {
        warn!(
            alg = ?header.alg,
            environment = %env.as_str(),
            "jwt_algorithm_rejected_by_hardening_profile"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    let providers = load_jwt_providers().map_err(|err| {
        warn!(error = %err, "jwt_provider_configuration_load_failed");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    let mut accepted: Option<(JwtClaims, String)> = None;
    for provider in providers {
        let selected_key = match select_key(&provider.keys, header.kid.as_deref()) {
            Some(key) => key,
            None => continue,
        };
        let decoding_key = match decoding_key_for_algorithm(header.alg, selected_key) {
            Ok(key) => key,
            Err(err) => {
                warn!(
                    provider = provider.name.as_deref().unwrap_or("provider"),
                    alg = ?header.alg,
                    error = %err,
                    "jwt_provider_key_material_invalid_for_algorithm"
                );
                continue;
            }
        };

        let mut validation = jsonwebtoken::Validation::new(header.alg);
        validation.algorithms = allowed_algs.clone();
        validation.validate_exp = true;
        validation.validate_aud = false;
        validation.leeway = jwt_leeway_secs();
        let token_data = match jsonwebtoken::decode::<JwtClaims>(bearer, &decoding_key, &validation)
        {
            Ok(data) => data,
            Err(_) => continue,
        };

        if validate_provider_claims(&token_data.claims, &provider).is_err() {
            continue;
        }

        accepted = Some((
            token_data.claims,
            provider.name.unwrap_or_else(|| "provider".to_string()),
        ));
        break;
    }

    let (claims, provider_name) = accepted.ok_or(StatusCode::UNAUTHORIZED)?;
    if matches!(claims.token_use.as_deref(), Some("refresh")) {
        warn!("refresh_token_presented_to_access_protected_route");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let scopes = scopes_from_claims(&claims);
    let roles = roles_from_claims(&claims);
    let tenant_id = tenant_from_claims(&claims);
    enforce_claim_policy(path, &claims, tenant_id.as_deref(), &scopes, &roles)?;

    Ok(AuthContext {
        subject: claims.sub,
        issuer: claims.iss,
        provider: Some(provider_name),
        token_id: claims.jti,
        token_use: claims.token_use,
        tenant_id,
        scopes,
        roles,
    })
}

pub async fn auth_middleware<S>(
    State(state): State<S>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode>
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let path = req.uri().path();

    let open = path == "/"
        || path == "/health"
        || path == "/ready"
        || path == "/contact"
        || path == "/api/contact"
        || path == "/api/v1/auth/login"
        || path == "/api/v1/auth/refresh"
        || path == "/api/v1/auth/revoke"
        || path == "/api/v1/auth/jwks"
        || path == "/api/v1/auth/capabilities"
        || path == "/api/v1/auth/status"
        || path == "/api/status"
        || path == "/metrics"
        || path == "/metrics/prometheus"
        || path == "/data/dashboard"
        || path == "/rpc/version"
        || path == "/rpc/now"
        || path == "/asset-manifest.json"
        || path.starts_with("/blog/")
        || path.starts_with("/pkg/");

    let is_public = state.runtime_state().public_paths.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            path.starts_with(prefix)
        } else {
            path == pattern
        }
    });

    if open || is_public {
        return Ok(next.run(req).await);
    }

    let mode = state.runtime_state().auth_mode.clone();
    let authorized = if mode.eq_ignore_ascii_case("jwt") || mode.eq_ignore_ascii_case("oidc") {
        authorize_with_jwt(&req, path)
    } else {
        authorize_with_static_bearer(&req)
    };
    let authorized = match authorized {
        Ok(ctx) => {
            if let Some(token_id) = ctx.token_id.as_deref() {
                let revoked_key = format!("auth:revoked:{token_id}");
                match state.runtime_state().store.get(&revoked_key).await {
                    Ok(Some(_)) => {
                        warn!(token_id = %token_id, path = %path, "revoked_token_rejected");
                        Err(StatusCode::UNAUTHORIZED)
                    }
                    Ok(None) => Ok(ctx),
                    Err(err) => {
                        warn!(
                            error = %err,
                            token_id = %token_id,
                            "revocation_lookup_failed_failing_closed"
                        );
                        Err(StatusCode::SERVICE_UNAVAILABLE)
                    }
                }
            } else {
                Ok(ctx)
            }
        }
        Err(code) => Err(code),
    };

    match authorized {
        Ok(ctx) => {
            if is_admin_api_path(path) && !has_admin_entitlement(&ctx) {
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(ctx);
            Ok(next.run(req).await)
        }
        Err(code) => {
            let runtime = state.runtime_state();
            runtime.auth_failures_total.fetch_add(1, Ordering::Relaxed);

            let client_ip = extract_client_ip(&req, runtime.trust_proxy_headers);
            let auth_window_secs = 60_u64;
            let auth_window = current_window_epoch(auth_window_secs);
            let auth_key = format!("auth:fail:{client_ip}:{auth_window}");

            let failures = match runtime.store.incr(&auth_key, 1).await {
                Ok(count) => count,
                Err(err) => {
                    warn!(
                        error = %err,
                        client_ip = %client_ip,
                        "auth_failure_store_error_failing_closed"
                    );
                    return Err(StatusCode::TOO_MANY_REQUESTS);
                }
            };
            if failures == 1 {
                let _ = runtime
                    .store
                    .expire(&auth_key, Duration::from_secs(auth_window_secs + 2))
                    .await;
            }

            if failures > 100 {
                warn!(
                    failures_in_window = failures,
                    window_seconds = 60,
                    limiter_scope = "per_ip_distributed_auth_failures",
                    client_ip = %client_ip,
                    "auth_failure_rate_limiter_triggered"
                );
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }

            Err(code)
        }
    }
}

pub fn is_internal_service_path(path: &str) -> bool {
    path.starts_with("/internal") || path.starts_with("/api/internal")
}

pub async fn service_auth_middleware<S>(
    State(state): State<S>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode>
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let path = req.uri().path().to_string();
    if !is_internal_service_path(&path) {
        return Ok(next.run(req).await);
    }

    let expected_scope = state.runtime_state().service_auth_scope.clone();
    let has_scope = req
        .extensions()
        .get::<AuthContext>()
        .map(|ctx| ctx.scopes.iter().any(|scope| scope == &expected_scope))
        .unwrap_or(false);

    if !has_scope {
        warn!(
            path = %path,
            required_scope = %expected_scope,
            "service_auth_scope_validation_failed"
        );
        return Err(StatusCode::FORBIDDEN);
    }

    debug!(
        path = %path,
        required_scope = %expected_scope,
        "service_auth_scope_validation_passed"
    );

    Ok(next.run(req).await)
}
