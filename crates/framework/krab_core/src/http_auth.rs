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

fn is_hmac_algorithm(alg: &jsonwebtoken::Algorithm) -> bool {
    matches!(
        alg,
        jsonwebtoken::Algorithm::HS256
            | jsonwebtoken::Algorithm::HS384
            | jsonwebtoken::Algorithm::HS512
    )
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

    // An allowlist mixing HMAC (HS*) with asymmetric (RS*/PS*/ES*/EdDSA)
    // families is the classic key-confusion footgun: with both allowed, an
    // attacker can take a public RSA/EC verification key and present it as an
    // HMAC secret. Fail closed rather than verify anything under such a
    // configuration; `KrabConfig::validate` rejects it at startup outside dev.
    let has_hmac = algorithms.iter().any(is_hmac_algorithm);
    let has_asymmetric = algorithms.iter().any(|alg| !is_hmac_algorithm(alg));
    if has_hmac && has_asymmetric {
        warn!(
            allowlist = %raw,
            "KRAB_JWT_ALLOWED_ALGS mixes HMAC and asymmetric algorithm families; refusing to verify"
        );
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

/// Parse `KRAB_AUTH_ROUTE_POLICIES_JSON`, distinguishing "not configured"
/// (`Ok(empty)`) from "configured but malformed" (`Err`). The enforcement path
/// treats the latter as a hard failure: a typo in the policy JSON must not
/// silently strip every route policy.
pub fn try_load_route_policies() -> Result<Vec<RoutePolicy>, serde_json::Error> {
    match std::env::var("KRAB_AUTH_ROUTE_POLICIES_JSON") {
        Ok(raw) => serde_json::from_str::<Vec<RoutePolicy>>(&raw),
        Err(_) => Ok(Vec::new()),
    }
}

pub fn load_route_policies() -> Vec<RoutePolicy> {
    try_load_route_policies().unwrap_or_else(|error| {
        tracing::error!(error = %error, "auth_route_policies_json_malformed");
        Vec::new()
    })
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

/// Coarse algorithm family, used to key pre-built decoding keys: material
/// that parses for one family (e.g. an RSA PEM) is reusable across every
/// algorithm in that family (RS256/RS384/RS512/PS256/...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum JwtAlgorithmFamily {
    Hmac,
    Rsa,
    Ec,
    Ed,
}

impl JwtAlgorithmFamily {
    const ALL: [Self; 4] = [Self::Hmac, Self::Rsa, Self::Ec, Self::Ed];

    fn of(algorithm: jsonwebtoken::Algorithm) -> Self {
        match algorithm {
            jsonwebtoken::Algorithm::HS256
            | jsonwebtoken::Algorithm::HS384
            | jsonwebtoken::Algorithm::HS512 => Self::Hmac,
            jsonwebtoken::Algorithm::RS256
            | jsonwebtoken::Algorithm::RS384
            | jsonwebtoken::Algorithm::RS512
            | jsonwebtoken::Algorithm::PS256
            | jsonwebtoken::Algorithm::PS384
            | jsonwebtoken::Algorithm::PS512 => Self::Rsa,
            jsonwebtoken::Algorithm::ES256 | jsonwebtoken::Algorithm::ES384 => Self::Ec,
            jsonwebtoken::Algorithm::EdDSA => Self::Ed,
        }
    }

    /// A representative algorithm for building a decoding key of this family.
    fn representative(self) -> jsonwebtoken::Algorithm {
        match self {
            Self::Hmac => jsonwebtoken::Algorithm::HS256,
            Self::Rsa => jsonwebtoken::Algorithm::RS256,
            Self::Ec => jsonwebtoken::Algorithm::ES256,
            Self::Ed => jsonwebtoken::Algorithm::EdDSA,
        }
    }
}

/// Parsed JWT provider configuration with decoding keys pre-built per
/// (provider index, kid, algorithm family). Building this once per
/// [`crate::http_runtime::RuntimeState`] takes provider JSON parsing and PEM
/// parsing off the per-request hot path.
///
/// Deliberately per-instance (no process-global caching): callers that mutate
/// the environment — tests, or services that reload state — construct a fresh
/// `RuntimeState` and therefore a fresh cache.
pub struct JwtVerifierCache {
    providers: Vec<JwtProviderConfig>,
    keys: std::collections::HashMap<(usize, String, JwtAlgorithmFamily), jsonwebtoken::DecodingKey>,
    load_failed: bool,
}

impl JwtVerifierCache {
    /// Build the cache from the current environment
    /// (`KRAB_JWT_PROVIDERS_JSON` / `KRAB_JWT_KEYS_JSON` / `KRAB_JWT_SECRET`).
    /// A malformed provider configuration is remembered as a load failure and
    /// surfaces as 503 on the request path, matching the previous per-request
    /// behavior.
    pub fn from_env() -> Self {
        match load_jwt_providers() {
            Ok(providers) => Self::from_providers(providers),
            Err(err) => {
                warn!(error = %err, "jwt_provider_configuration_load_failed");
                Self {
                    providers: Vec::new(),
                    keys: std::collections::HashMap::new(),
                    load_failed: true,
                }
            }
        }
    }

    pub fn from_providers(providers: Vec<JwtProviderConfig>) -> Self {
        let mut keys = std::collections::HashMap::new();
        for (provider_index, provider) in providers.iter().enumerate() {
            for (kid, material) in &provider.keys {
                for family in JwtAlgorithmFamily::ALL {
                    // Failure is expected for most (material, family) pairs —
                    // an HMAC secret is not an RSA PEM. The request path warns
                    // if a requested family ends up with no usable key.
                    if let Ok(key) = decoding_key_for_algorithm(family.representative(), material) {
                        keys.insert((provider_index, kid.clone(), family), key);
                    }
                }
            }
        }
        Self {
            providers,
            keys,
            load_failed: false,
        }
    }

    pub fn providers(&self) -> &[JwtProviderConfig] {
        &self.providers
    }

    pub fn load_failed(&self) -> bool {
        self.load_failed
    }

    fn decoding_key(
        &self,
        provider_index: usize,
        kid: &str,
        algorithm: jsonwebtoken::Algorithm,
    ) -> Option<&jsonwebtoken::DecodingKey> {
        self.keys.get(&(
            provider_index,
            kid.to_string(),
            JwtAlgorithmFamily::of(algorithm),
        ))
    }
}

/// Resolve which configured key id `select_key` would pick for this request,
/// so the pre-built decoding key can be looked up under the same identity.
fn effective_kid<'a>(provider: &'a JwtProviderConfig, kid: Option<&str>) -> Option<&'a str> {
    match kid {
        Some(k) => provider
            .keys
            .get_key_value(k)
            .map(|(name, _)| name.as_str()),
        None if require_kid() => None,
        None => provider
            .keys
            .get_key_value("default")
            .map(|(name, _)| name.as_str())
            .or_else(|| provider.keys.keys().next().map(String::as_str)),
    }
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

    // Fail closed on malformed policy JSON: returning an empty policy set here
    // would silently drop every configured restriction, while the sibling
    // KRAB_AUTH_REQUIRED_CLAIMS_JSON path below already rejects on bad JSON.
    let route_policies = try_load_route_policies().map_err(|error| {
        tracing::error!(
            error = %error,
            "auth_route_policies_json_malformed_failing_closed"
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    for policy in route_policies
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

/// Compatibility wrapper: builds a [`JwtVerifierCache`] from the environment
/// on every call, matching the pre-cache behavior. Prefer
/// [`authorize_with_jwt_cached`] with the cache held on
/// [`crate::http_runtime::RuntimeState`] — that is what `auth_middleware`
/// uses — so provider JSON and PEM key material are parsed once, not per
/// request.
pub fn authorize_with_jwt(req: &Request<Body>, path: &str) -> Result<AuthContext, StatusCode> {
    let cache = JwtVerifierCache::from_env();
    authorize_with_jwt_cached(req, path, &cache)
}

pub fn authorize_with_jwt_cached(
    req: &Request<Body>,
    path: &str,
    cache: &JwtVerifierCache,
) -> Result<AuthContext, StatusCode> {
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

    if cache.load_failed() {
        warn!("jwt_provider_configuration_unavailable_rejecting_request");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    let mut accepted: Option<(JwtClaims, String)> = None;
    for (provider_index, provider) in cache.providers().iter().enumerate() {
        let selected_kid = match effective_kid(provider, header.kid.as_deref()) {
            Some(kid) => kid,
            None => continue,
        };
        let decoding_key = match cache.decoding_key(provider_index, selected_kid, header.alg) {
            Some(key) => key,
            None => {
                warn!(
                    provider = provider.name.as_deref().unwrap_or("provider"),
                    alg = ?header.alg,
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
        let token_data = match jsonwebtoken::decode::<JwtClaims>(bearer, decoding_key, &validation)
        {
            Ok(data) => data,
            Err(_) => continue,
        };

        if validate_provider_claims(&token_data.claims, provider).is_err() {
            continue;
        }

        accepted = Some((
            token_data.claims,
            provider
                .name
                .clone()
                .unwrap_or_else(|| "provider".to_string()),
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

/// Baseline unauthenticated ("open") path patterns, used when
/// `KRAB_AUTH_OPEN_PATHS` is unset. A trailing `*` makes a pattern a prefix
/// match; anything else is an exact match. This is the list that used to be
/// hardcoded in `auth_middleware`, minus the metrics endpoints — see
/// [`METRICS_OPEN_PATHS`].
pub(crate) const DEFAULT_AUTH_OPEN_PATHS: &[&str] = &[
    "/",
    "/health",
    "/ready",
    "/contact",
    "/api/contact",
    "/api/v1/auth/login",
    "/api/v1/auth/refresh",
    "/api/v1/auth/revoke",
    "/api/v1/auth/jwks",
    "/api/v1/auth/capabilities",
    "/api/v1/auth/status",
    "/api/status",
    "/data/dashboard",
    "/rpc/version",
    "/rpc/now",
    "/asset-manifest.json",
    "/blog/*",
    "/pkg/*",
];

/// Telemetry endpoints, anonymous only when `KRAB_METRICS_PUBLIC` is on.
///
/// These shipped on the default open-path list, which meant every service
/// built on Krab handed an unauthenticated caller its full route inventory,
/// request volumes, error counts, and latency histograms — a free
/// reconnaissance map of the deployment, and on low-traffic services enough
/// per-route timing to infer individual user activity. Nothing about a
/// framework default should require an operator to notice a leak and close
/// it; the safe state is the one you get by not configuring anything.
///
/// Closing it by deleting the entries would have been the wrong repair.
/// `KRAB_AUTH_OPEN_PATHS` REPLACES the baseline list rather than extending
/// it, so the only way back for someone scraping today would have been to
/// restate all eighteen surviving defaults and hope none were missed or
/// mistyped — an upgrade step that silently closes `/health` if you get it
/// wrong. A dedicated flag keeps the restore to one unambiguous line,
/// `KRAB_METRICS_PUBLIC=true`, and keeps the decision auditable: a grep for
/// that variable across an estate answers "who is exposing metrics?", which
/// a hand-copied path list never could.
///
/// The flag is deliberately additive over whatever `auth_open_path_patterns`
/// resolves, including an explicit list. Each knob then means exactly what
/// its name says — one governs the general open surface, one governs the
/// metrics endpoints — instead of the flag being silently inert whenever
/// `KRAB_AUTH_OPEN_PATHS` happens to be set, which is the sort of hidden
/// interaction that gets a service published to the internet by accident.
pub(crate) const METRICS_OPEN_PATHS: &[&str] = &["/metrics", "/metrics/prometheus"];

/// Resolve the open-path pattern list: `KRAB_AUTH_OPEN_PATHS` when set
/// (comma-separated; an explicitly EMPTY value closes every default open
/// path), the baseline list otherwise. [`METRICS_OPEN_PATHS`] is appended in
/// either case when `KRAB_METRICS_PUBLIC` is on.
pub(crate) fn auth_open_path_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = match std::env::var("KRAB_AUTH_OPEN_PATHS") {
        Ok(raw) => parse_csv_set(&raw),
        Err(_) => DEFAULT_AUTH_OPEN_PATHS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };

    if metrics_public() {
        for path in METRICS_OPEN_PATHS {
            let path = (*path).to_string();
            if !patterns.contains(&path) {
                patterns.push(path);
            }
        }
    }

    patterns
}

/// Whether the metrics endpoints are anonymously scrapeable. Off by default.
pub(crate) fn metrics_public() -> bool {
    crate::http::bool_env("KRAB_METRICS_PUBLIC", false)
}

/// Match `path` against patterns: trailing `*` is a prefix match, everything
/// else exact. Shared by the open-path list and `KRAB_AUTH_PUBLIC_PATHS`.
pub(crate) fn path_matches_patterns(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if let Some(prefix) = pattern.strip_suffix('*') {
            path.starts_with(prefix)
        } else {
            path == pattern
        }
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

    let open = path_matches_patterns(path, &auth_open_path_patterns());
    let is_public = path_matches_patterns(path, &state.runtime_state().public_paths);

    if open || is_public {
        return Ok(next.run(req).await);
    }

    let mode = state.runtime_state().auth_mode.clone();
    let authorized = if mode.eq_ignore_ascii_case("jwt") || mode.eq_ignore_ascii_case("oidc") {
        authorize_with_jwt_cached(&req, path, &state.runtime_state().jwt_verifier_cache)
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
            let auth_window_secs = runtime.auth_fail_window_secs;
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

            if failures > runtime.auth_fail_threshold {
                warn!(
                    failures_in_window = failures,
                    window_seconds = auth_window_secs,
                    threshold = runtime.auth_fail_threshold,
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

#[cfg(test)]
mod open_path_tests {
    use super::{auth_open_path_patterns, path_matches_patterns, DEFAULT_AUTH_OPEN_PATHS};
    use serial_test::serial;

    /// Every test here resolves the pattern list from the environment, so both
    /// knobs must start unset regardless of what ran before.
    fn reset_open_path_env() {
        std::env::remove_var("KRAB_AUTH_OPEN_PATHS");
        std::env::remove_var("KRAB_METRICS_PUBLIC");
    }

    #[test]
    fn pattern_matching_supports_exact_and_prefix() {
        let patterns: Vec<String> = vec!["/health".into(), "/blog/*".into()];

        assert!(path_matches_patterns("/health", &patterns));
        assert!(!path_matches_patterns("/healthz", &patterns));
        assert!(path_matches_patterns("/blog/post-1", &patterns));
        assert!(!path_matches_patterns("/metrics", &patterns));
    }

    #[test]
    #[serial]
    fn default_open_paths_exclude_metrics() {
        reset_open_path_env();
        let patterns = auth_open_path_patterns();

        assert_eq!(
            patterns,
            DEFAULT_AUTH_OPEN_PATHS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert!(
            !path_matches_patterns("/metrics", &patterns),
            "metrics must not be anonymously readable without an explicit opt-in"
        );
        assert!(!path_matches_patterns("/metrics/prometheus", &patterns));
        assert!(path_matches_patterns("/health", &patterns));
        assert!(path_matches_patterns("/pkg/app_bg.wasm", &patterns));
        assert!(!path_matches_patterns("/api/v1/users", &patterns));
    }

    #[test]
    #[serial]
    fn metrics_public_env_reopens_both_metrics_paths() {
        reset_open_path_env();
        std::env::set_var("KRAB_METRICS_PUBLIC", "true");
        let patterns = auth_open_path_patterns();
        reset_open_path_env();

        assert!(path_matches_patterns("/metrics", &patterns));
        assert!(path_matches_patterns("/metrics/prometheus", &patterns));
        // The opt-in must not disturb anything else on the baseline list.
        assert!(path_matches_patterns("/health", &patterns));
        assert!(!path_matches_patterns("/api/v1/users", &patterns));
    }

    #[test]
    #[serial]
    fn metrics_public_env_applies_over_an_explicit_open_path_list() {
        reset_open_path_env();
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/health,/ready");
        std::env::set_var("KRAB_METRICS_PUBLIC", "1");
        let patterns = auth_open_path_patterns();
        reset_open_path_env();

        assert!(path_matches_patterns("/health", &patterns));
        assert!(
            path_matches_patterns("/metrics", &patterns),
            "the metrics flag must not be silently inert when KRAB_AUTH_OPEN_PATHS is set"
        );
    }

    #[test]
    #[serial]
    fn operators_can_close_metrics_via_env() {
        reset_open_path_env();
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "/health,/ready");
        let patterns = auth_open_path_patterns();
        reset_open_path_env();

        assert!(path_matches_patterns("/health", &patterns));
        assert!(
            !path_matches_patterns("/metrics", &patterns),
            "an explicit open-path list must be able to close /metrics"
        );
    }

    #[test]
    #[serial]
    fn empty_env_value_closes_every_default_open_path() {
        reset_open_path_env();
        std::env::set_var("KRAB_AUTH_OPEN_PATHS", "");
        let patterns = auth_open_path_patterns();
        reset_open_path_env();

        assert!(patterns.is_empty());
        assert!(!path_matches_patterns("/health", &patterns));
    }
}
