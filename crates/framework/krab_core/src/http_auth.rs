//! Request authentication and authorisation for the `rest` HTTP stack.
//!
//! `KRAB_AUTH_MODE=jwt` (or `oidc`) verifies bearer JWTs against one or more
//! [`JwtProviderConfig`]s — static keys, a remote JWKS, or both — then applies
//! the [`AuthPolicy`] (required scopes and roles, admin entitlement, tenant
//! rules, [`RoutePolicy`]s, required claims). `static` compares a fixed bearer
//! token and is dev-only. A request that passes gets an [`AuthContext`] in its
//! extensions; one that fails is counted by [`AuthFailureReason`] and answered
//! with a bare status code.
//!
//! [`auth_middleware`] and [`service_auth_middleware`] are installed by
//! [`crate::http::apply_common_http_layers`]; the free functions are the
//! pieces they are built from, exposed for services and tests.

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

/// The authenticated caller, inserted into the request extensions by
/// [`auth_middleware`] when a request authenticates. Absent on open and
/// public paths.
///
/// Under static bearer auth the identity is fixed: subject
/// `static-token-client`, issuer `krab.static`, provider `static`, no scopes
/// or roles.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// The token's `sub` claim.
    pub subject: Option<String>,
    /// The token's `iss` claim.
    pub issuer: Option<String>,
    /// Name of the [`JwtProviderConfig`] that verified the token (`provider`
    /// when the configuration names none).
    pub provider: Option<String>,
    /// The token's `jti` claim; the key checked against the revocation list.
    pub token_id: Option<String>,
    /// The token's `token_use` claim. Tokens with `token_use: "refresh"` are
    /// rejected before an `AuthContext` is built.
    pub token_use: Option<String>,
    /// The tenant, from [`tenant_from_claims`].
    pub tenant_id: Option<String>,
    /// The granted scopes, from [`scopes_from_claims`].
    pub scopes: Vec<String>,
    /// The granted roles, from [`roles_from_claims`].
    pub roles: Vec<String>,
}

/// Whether `ctx` carries the admin scope (`KRAB_AUTH_ADMIN_SCOPE`, default
/// `admin`) or the admin role (`KRAB_AUTH_ADMIN_ROLE`, default `admin`).
///
/// Reads the environment on every call; the middleware uses
/// [`AuthPolicy::has_admin_entitlement`] on its startup snapshot instead.
pub fn has_admin_entitlement(ctx: &AuthContext) -> bool {
    let admin_scope =
        std::env::var("KRAB_AUTH_ADMIN_SCOPE").unwrap_or_else(|_| "admin".to_string());
    let admin_role = std::env::var("KRAB_AUTH_ADMIN_ROLE").unwrap_or_else(|_| "admin".to_string());
    ctx.scopes.iter().any(|s| s == &admin_scope) || ctx.roles.iter().any(|r| r == &admin_role)
}

/// The JWT claims Krab reads. Every standard field is optional at the type
/// level; which ones a token must carry is decided by verification (`exp` is
/// always validated) and by the provider and [`AuthPolicy`] configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JwtClaims {
    /// Subject: who the token identifies.
    pub sub: Option<String>,
    /// Issuer; must equal the provider's `issuer` when one is configured.
    pub iss: Option<String>,
    /// Audience, a string or an array of strings; must contain the
    /// provider's `audience` when one is configured.
    pub aud: Option<Value>,
    /// Expiry, in Unix seconds. Verified, with [`jwt_leeway_secs`] of leeway.
    pub exp: Option<i64>,
    /// Token id; used for revocation.
    pub jti: Option<String>,
    /// What the token is for. A token with `token_use: "refresh"` is rejected
    /// by [`auth_middleware`]; any other value, or none, is accepted.
    pub token_use: Option<String>,
    /// Tenant id, short form. [`tenant_from_claims`] prefers `tenant_id`.
    pub tid: Option<String>,
    /// Tenant id.
    pub tenant_id: Option<String>,
    /// Space-separated scopes (OAuth 2.0 `scope`). Takes precedence over
    /// `scp`.
    pub scope: Option<String>,
    /// Scopes as a space-separated string or an array (the Azure AD `scp`
    /// form).
    pub scp: Option<Value>,
    /// Roles. Takes precedence over `role`.
    pub roles: Option<Vec<String>>,
    /// A single role.
    pub role: Option<String>,
    /// Every other claim in the token, by name.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}

/// One trusted token issuer, as configured in `KRAB_JWT_PROVIDERS_JSON` (an
/// array of these), or synthesised by [`load_jwt_providers`] from the
/// single-provider variables.
///
/// A provider with neither `keys` nor `jwks_url` is dropped when loaded.
#[derive(Debug, Clone, Deserialize)]
pub struct JwtProviderConfig {
    /// Label used in logs and in [`AuthContext::provider`].
    #[serde(default)]
    pub name: Option<String>,
    /// Required `iss` value. When `None`, issuer is not checked —
    /// [`KrabConfig::validate`](crate::config::KrabConfig::validate) therefore
    /// requires it outside `dev`.
    #[serde(default)]
    pub issuer: Option<String>,
    /// Required `aud` value. When `None`, audience is not checked — and, like
    /// `issuer`, it is required outside `dev`.
    #[serde(default)]
    pub audience: Option<String>,
    /// Static verification keys by `kid`. May be empty when `jwks_url` is set.
    ///
    /// Each value is an HMAC secret or a PEM public key (RSA, EC or Ed25519);
    /// which algorithm family it serves is worked out from what it parses as.
    #[serde(default)]
    pub keys: std::collections::BTreeMap<String, String>,
    /// Claims the token must carry with exactly these JSON values, in
    /// addition to the global `KRAB_AUTH_REQUIRED_CLAIMS_JSON`.
    #[serde(default)]
    pub required_claims: std::collections::BTreeMap<String, Value>,
    /// URL of the provider's published JSON Web Key Set, fetched and refreshed
    /// by the `jwks` module. `https://` only outside `dev`. Added in 0.6.0.
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// `kid` -> time from which that key no longer verifies, as RFC 3339
    /// (`2026-10-01T00:00:00Z`) or Unix seconds. Retires a rotated-out key on
    /// a schedule instead of on the next deploy. Added in 0.6.0.
    #[serde(default)]
    pub key_not_after: std::collections::BTreeMap<String, String>,
}

/// An authorisation rule for every path under a prefix, from the
/// `KRAB_AUTH_ROUTE_POLICIES_JSON` array.
///
/// Every policy whose `prefix` matches the request path applies, and each of
/// its non-empty conditions must hold; a failed condition answers `401`.
/// Empty lists impose nothing.
#[derive(Debug, Clone, Deserialize)]
pub struct RoutePolicy {
    /// Path prefix the policy applies to (plain string prefix, no wildcards).
    pub prefix: String,
    /// Scopes the token must all have.
    #[serde(default)]
    pub all_scopes: Vec<String>,
    /// Scopes of which the token must have at least one.
    #[serde(default)]
    pub any_scopes: Vec<String>,
    /// Roles the token must all have.
    #[serde(default)]
    pub all_roles: Vec<String>,
    /// Roles of which the token must have at least one.
    #[serde(default)]
    pub any_roles: Vec<String>,
    /// When non-empty, the token's `sub` must be one of these.
    #[serde(default)]
    pub allow_subjects: Vec<String>,
    /// Require the path to contain `/tenants/{id}` and the token's tenant to
    /// equal that id.
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

/// Whether the JWT algorithm named `alg` (for example `RS256`,
/// case-insensitive) is on the `KRAB_JWT_ALLOWED_ALGS` allowlist.
///
/// The allowlist defaults to `HS256` alone. It is false for an unknown name,
/// and for every name when the allowlist is empty after parsing or mixes HMAC
/// with asymmetric families (fail closed). `_environment` is currently
/// unused: the same allowlist applies everywhere. Reads the environment on
/// every call.
pub fn jwt_algorithm_allowed(alg: &str, _environment: &crate::config::Environment) -> bool {
    let Some(candidate) = parse_jwt_algorithm(alg) else {
        return false;
    };

    configured_jwt_algorithms()
        .map(|allowed| allowed.contains(&candidate))
        .unwrap_or(false)
}

/// Clock-skew allowance for `exp`, in seconds: `KRAB_JWT_LEEWAY_SECS`,
/// default 30 (also used when the value does not parse).
pub fn jwt_leeway_secs() -> u64 {
    std::env::var("KRAB_JWT_LEEWAY_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30)
}

/// The single-provider verification keys, by `kid`.
///
/// `KRAB_JWT_KEYS_JSON` (a `{"kid": "key material"}` object) when set and
/// non-empty; otherwise `KRAB_JWT_SECRET` under the kid `default`; otherwise
/// an empty map, with a warning. Both are read through
/// [`read_env_or_file`](crate::config::read_env_or_file), so the `_FILE`
/// forms work. Errors when a secret cannot be read or the JSON is malformed.
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

/// The configured JWT providers.
///
/// `KRAB_JWT_PROVIDERS_JSON` (via
/// [`read_env_or_file`](crate::config::read_env_or_file)) when it yields at
/// least one provider with keys or a `jwks_url`. Otherwise a single provider
/// named `default`, built from `KRAB_OIDC_ISSUER`, `KRAB_OIDC_AUDIENCE`,
/// `KRAB_OIDC_JWKS_URL`, `KRAB_JWT_KEY_NOT_AFTER_JSON` and
/// [`load_rotation_keys`] (static keys are skipped when only a JWKS URL is
/// configured). Errors when a secret cannot be read or any JSON is malformed.
pub fn load_jwt_providers() -> anyhow::Result<Vec<JwtProviderConfig>> {
    if let Some(raw) = crate::config::read_env_or_file("KRAB_JWT_PROVIDERS_JSON")? {
        let mut providers = serde_json::from_str::<Vec<JwtProviderConfig>>(&raw)?;
        providers.retain(|p| {
            !p.keys.is_empty() || p.jwks_url.as_deref().is_some_and(|u| !u.trim().is_empty())
        });
        if !providers.is_empty() {
            return Ok(providers);
        }
    }

    let jwks_url = std::env::var("KRAB_OIDC_JWKS_URL")
        .ok()
        .filter(|url| !url.trim().is_empty());
    let key_not_after = match std::env::var("KRAB_JWT_KEY_NOT_AFTER_JSON") {
        Ok(raw) if !raw.trim().is_empty() => {
            let parsed: std::collections::BTreeMap<String, Value> = serde_json::from_str(&raw)?;
            parsed
                .into_iter()
                .map(|(kid, value)| {
                    let value = match value {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (kid, value)
                })
                .collect()
        }
        _ => std::collections::BTreeMap::new(),
    };
    // With a remote key set, static keys are optional; without one, an empty
    // key map still warns inside `load_rotation_keys`, as before.
    let keys = if jwks_url.is_some()
        && crate::config::read_env_or_file("KRAB_JWT_KEYS_JSON")?.is_none()
        && crate::config::read_env_or_file("KRAB_JWT_SECRET")?.is_none()
    {
        std::collections::BTreeMap::new()
    } else {
        load_rotation_keys()?
    };

    Ok(vec![JwtProviderConfig {
        name: Some("default".to_string()),
        issuer: std::env::var("KRAB_OIDC_ISSUER").ok(),
        audience: std::env::var("KRAB_OIDC_AUDIENCE").ok(),
        keys,
        required_claims: std::collections::BTreeMap::new(),
        jwks_url,
        key_not_after,
    }])
}

/// Most signature verifications a token without a `kid` may cost: it is tried
/// against at most this many keys, across all providers, `default` first,
/// then in key-id order. Past that it is rejected (`401`, `unknown_key`)
/// without trying the rest — each trial is a full signature verification,
/// so an unbounded trial is a CPU amplifier. Deployments with more keys
/// should have tokens name their `kid` (or set `KRAB_JWT_REQUIRE_KID`).
pub const MAX_KIDLESS_KEY_TRIALS: usize = 8;

/// Whether tokens must name their key in a `kid` header
/// (`KRAB_JWT_REQUIRE_KID`, `1` or `true`; default false). When false, a
/// token without `kid` is tried against every key, `default` first, up to
/// [`MAX_KIDLESS_KEY_TRIALS`] keys.
pub fn require_kid() -> bool {
    std::env::var("KRAB_JWT_REQUIRE_KID")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Picks one key from `keys` for a token's `kid`: the exact match when `kid`
/// is given; with no `kid`, `None` if [`require_kid`], else the `default` key
/// or, failing that, the first by sort order.
///
/// Krab's own verifier does not use this: it tries a kid-less token against
/// every key rather than one.
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

/// The token's tenant: the first of `tenant_id` and `tid` that is present
/// (string values only, for the flattened extras).
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

/// The segment following the first `tenants` segment of `path`, if any —
/// `acme` for `/api/v1/tenants/acme/users`.
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

/// Lenient form of [`try_load_route_policies`]: malformed JSON logs an error
/// and yields no policies. Enforcement does not use this — it fails closed on
/// malformed JSON instead.
pub fn load_route_policies() -> Vec<RoutePolicy> {
    try_load_route_policies().unwrap_or_else(|error| {
        tracing::error!(error = %error, "auth_route_policies_json_malformed");
        Vec::new()
    })
}

/// Checks a verified token's claims against `provider`: `iss` must equal
/// its `issuer` and `aud` must equal or contain its `audience` (each only
/// when configured), and every `required_claims` entry must be present with
/// an equal value. `Err(401)` on the first mismatch.
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

/// The token's scopes: `scope` split on whitespace if present, else `scp`
/// (a whitespace-separated string or an array of strings), else none.
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

/// The token's roles: `roles` if present, else the single `role`, else none.
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
pub(crate) enum JwtAlgorithmFamily {
    Hmac,
    Rsa,
    Ec,
    Ed,
}

impl JwtAlgorithmFamily {
    const ALL: [Self; 4] = [Self::Hmac, Self::Rsa, Self::Ec, Self::Ed];

    pub(crate) fn of(algorithm: jsonwebtoken::Algorithm) -> Self {
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
/// Providers with a `jwks_url` hold a the `jwks` module source instead of (or as
/// well as) static keys; see that module for fetch and refresh behaviour.
///
/// Deliberately per-instance (no process-global caching): callers that mutate
/// the environment — tests, or services that reload state — construct a fresh
/// `RuntimeState` and therefore a fresh cache.
pub struct JwtVerifierCache {
    providers: Vec<JwtProviderConfig>,
    keys: std::collections::HashMap<(usize, String, JwtAlgorithmFamily), jsonwebtoken::DecodingKey>,
    /// Per provider index: its remote key set, if it has a `jwks_url`.
    jwks: Vec<Option<std::sync::Arc<crate::jwks::JwksSource>>>,
    /// Per provider index: `kid` -> Unix second from which the key no longer
    /// verifies (`key_not_after`).
    retire_at: Vec<std::collections::HashMap<String, i64>>,
    load_failed: bool,
    /// Whether [`JwtVerifierCache::spawn_background_refresh`] scheduled a
    /// refresh task. Without one, remote key sets are refreshed lazily on
    /// the request path instead.
    background_refresh: std::sync::atomic::AtomicBool,
}

impl JwtVerifierCache {
    /// Build the cache from the current environment
    /// (`KRAB_JWT_PROVIDERS_JSON` / `KRAB_JWT_KEYS_JSON` / `KRAB_JWT_SECRET` /
    /// `KRAB_OIDC_JWKS_URL` / `KRAB_JWT_KEY_NOT_AFTER_JSON`). A malformed
    /// provider configuration is remembered as a load failure and surfaces as
    /// 503 on the request path, matching the previous per-request behavior.
    pub fn from_env() -> Self {
        match load_jwt_providers() {
            Ok(providers) => Self::from_providers(providers),
            Err(err) => {
                warn!(error = %err, "jwt_provider_configuration_load_failed");
                Self::failed()
            }
        }
    }

    fn failed() -> Self {
        Self {
            providers: Vec::new(),
            keys: std::collections::HashMap::new(),
            jwks: Vec::new(),
            retire_at: Vec::new(),
            load_failed: true,
            background_refresh: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Build the cache from explicit provider configurations, pre-parsing
    /// every static key for each algorithm family it is valid for.
    ///
    /// Still reads `KRAB_ENVIRONMENT` and the JWKS settings from the
    /// environment. Fails closed — every request then gets `503` — when a
    /// `jwks_url` is not `https://` outside `dev`, its HTTP client cannot be
    /// built, or a `key_not_after` value cannot be parsed. Remote key sets
    /// are not fetched here.
    pub fn from_providers(providers: Vec<JwtProviderConfig>) -> Self {
        Self::from_providers_with(providers, crate::jwks::JwksSettings::from_env())
    }

    /// [`JwtVerifierCache::from_providers`] with explicit JWKS settings.
    pub(crate) fn from_providers_with(
        providers: Vec<JwtProviderConfig>,
        settings: crate::jwks::JwksSettings,
    ) -> Self {
        let environment = crate::config::Environment::from_env();
        let mut keys = std::collections::HashMap::new();
        let mut jwks = Vec::with_capacity(providers.len());
        let mut retire_at = Vec::with_capacity(providers.len());

        for (provider_index, provider) in providers.iter().enumerate() {
            let name = provider.name.as_deref().unwrap_or("provider");
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

            match provider.jwks_url.as_deref().map(str::trim) {
                Some(url) if !url.is_empty() => {
                    if !crate::jwks::url_allowed(url, &environment) {
                        // Fail closed: keys fetched over plain HTTP can be
                        // swapped by anyone on the path.
                        tracing::error!(
                            provider = name,
                            url,
                            environment = %environment.as_str(),
                            "jwks_url_rejected_https_required_outside_dev"
                        );
                        return Self::failed();
                    }
                    match crate::jwks::JwksSource::new(url.to_string(), settings) {
                        Ok(source) => jwks.push(Some(std::sync::Arc::new(source))),
                        Err(error) => {
                            // Fail closed: a default client would follow
                            // redirects and have no timeout.
                            tracing::error!(
                                provider = name,
                                url,
                                %error,
                                "jwks_source_unusable_http_client_build_failed"
                            );
                            return Self::failed();
                        }
                    }
                }
                _ => jwks.push(None),
            }

            let mut retire = std::collections::HashMap::new();
            for (kid, raw) in &provider.key_not_after {
                let Some(at) = crate::jwks::parse_not_after(raw) else {
                    // A retirement that cannot be read must not silently
                    // become "never retires".
                    tracing::error!(
                        provider = name,
                        kid,
                        value = raw,
                        "jwt_key_not_after_unparseable"
                    );
                    return Self::failed();
                };
                retire.insert(kid.clone(), at);
            }
            retire_at.push(retire);
        }

        Self {
            providers,
            keys,
            jwks,
            retire_at,
            load_failed: false,
            background_refresh: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The providers tokens are verified against, in the order they are
    /// tried. Empty when loading failed.
    pub fn providers(&self) -> &[JwtProviderConfig] {
        &self.providers
    }

    /// Whether the provider configuration could not be loaded. Every JWT
    /// request is then rejected with `503` rather than verified against an
    /// empty key set.
    pub fn load_failed(&self) -> bool {
        self.load_failed
    }

    /// Whether any provider verifies against a remote key set.
    pub fn uses_jwks(&self) -> bool {
        self.jwks.iter().any(Option::is_some)
    }

    /// Make sure every remote key set can verify a token naming `kid`:
    /// fetch a set that has never loaded, and refetch one that lacks `kid`,
    /// each within the refetch rate limit. When no background refresh is
    /// running (see [`JwtVerifierCache::spawn_background_refresh`]), also
    /// refetch a set older than `KRAB_OIDC_JWKS_REFRESH_SECS`. Fetch failures
    /// are logged; the verification that follows reports what is still
    /// missing.
    pub async fn prepare_for_kid(&self, kid: Option<&str>) {
        let periodic = !self
            .background_refresh
            .load(std::sync::atomic::Ordering::Relaxed);
        for source in self.jwks.iter().flatten() {
            if let Some(Err(error)) = source.refresh_if_needed(kid, periodic).await {
                warn!(url = %source.url(), %error, "jwks_fetch_failed");
            }
        }
    }

    /// Refresh every remote key set now.
    pub async fn refresh_jwks(&self) {
        for source in self.jwks.iter().flatten() {
            if let Err(error) = source.refresh().await {
                warn!(url = %source.url(), %error, "jwks_refresh_failed");
            }
        }
    }

    /// Keep remote key sets fresh in the background for as long as `cache`
    /// is alive. A no-op without remote key sets.
    ///
    /// Outside a Tokio runtime no task can be spawned: a warning is logged
    /// and the key sets are refreshed lazily instead — the first request
    /// after `KRAB_OIDC_JWKS_REFRESH_SECS` refetches them — so a key removed
    /// at the provider still stops verifying.
    pub fn spawn_background_refresh(cache: &std::sync::Arc<Self>) {
        if !cache.uses_jwks() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                refresh = "lazy_on_request_path",
                "jwks_background_refresh_unavailable_outside_tokio_runtime"
            );
            return;
        };
        cache
            .background_refresh
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let interval = cache
            .jwks
            .iter()
            .flatten()
            .map(|s| s.settings().refresh_every)
            .min()
            .unwrap_or(std::time::Duration::from_secs(300));
        let weak = std::sync::Arc::downgrade(cache);
        handle.spawn(async move {
            loop {
                let Some(cache) = weak.upgrade() else {
                    return;
                };
                cache.refresh_jwks().await;
                drop(cache);
                tokio::time::sleep(interval).await;
            }
        });
    }

    fn decoding_key(
        &self,
        provider_index: usize,
        kid: &str,
        algorithm: jsonwebtoken::Algorithm,
    ) -> Option<jsonwebtoken::DecodingKey> {
        let family = JwtAlgorithmFamily::of(algorithm);
        if let Some(key) = self.keys.get(&(provider_index, kid.to_string(), family)) {
            return Some(key.clone());
        }
        self.jwks.get(provider_index)?.as_ref()?.key(kid, family)
    }

    /// The key ids a token may have been signed with, in the order to try
    /// them, across static keys and the provider's remote key set.
    ///
    /// A token naming a `kid` is tried against that key only. A token with no
    /// `kid` (allowed unless `KRAB_JWT_REQUIRE_KID` is on) is tried against
    /// every key, `default` first (the verifier stops after
    /// [`MAX_KIDLESS_KEY_TRIALS`]). Before 0.6.0 it was tried against one key
    /// — `default`, or failing that whichever sorted first — so during a
    /// rotation, a kid-less token signed with the other key was rejected
    /// however valid it was.
    fn candidate_kids(&self, provider_index: usize, kid: Option<&str>) -> Vec<String> {
        let provider = &self.providers[provider_index];
        let remote = self.jwks.get(provider_index).and_then(Option::as_ref);
        match kid {
            Some(k) => {
                let known =
                    provider.keys.contains_key(k) || remote.is_some_and(|source| source.has_kid(k));
                if known {
                    vec![k.to_string()]
                } else {
                    Vec::new()
                }
            }
            None if require_kid() => Vec::new(),
            None => {
                let mut kids: Vec<String> = provider.keys.keys().cloned().collect();
                if let Some(source) = remote {
                    kids.extend(source.kids());
                }
                if let Some(pos) = kids.iter().position(|k| k == "default") {
                    let default = kids.remove(pos);
                    kids.insert(0, default);
                }
                kids
            }
        }
    }

    /// Whether `kid` of provider `provider_index` is past its `key_not_after`.
    fn is_retired(&self, provider_index: usize, kid: &str, now: i64) -> bool {
        self.retire_at
            .get(provider_index)
            .and_then(|map| map.get(kid))
            .is_some_and(|at| now >= *at)
    }

    /// Whether provider `provider_index` has a remote key set that has never
    /// loaded — nothing to verify with, through no fault of the caller.
    fn remote_unavailable(&self, provider_index: usize) -> bool {
        self.jwks
            .get(provider_index)
            .and_then(Option::as_ref)
            .is_some_and(|source| !source.loaded())
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

/// Why a request failed authentication. The `reason` label of
/// `krab_auth_failures_by_reason_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthFailureReason {
    /// No `Authorization: Bearer` credential at all.
    MissingCredentials,
    /// The token does not decode (not a JWT, bad base64, bad JSON).
    MalformedToken,
    /// The token's `alg` is not on the allowlist.
    AlgorithmRejected,
    /// No configured or published key has the token's `kid`.
    UnknownKey,
    /// The key the token names is past its `key_not_after`.
    KeyRetired,
    /// The signature does not verify.
    InvalidSignature,
    /// The token is expired (beyond the leeway).
    Expired,
    /// Issuer, audience or provider-required claims did not match.
    ClaimsRejected,
    /// A refresh token was presented to an access-protected route.
    RefreshTokenRejected,
    /// A scope, role, tenant or route policy denied the request.
    PolicyDenied,
    /// The token's `jti` is revoked.
    Revoked,
    /// A static bearer token did not match.
    CredentialMismatch,
    /// Verification is impossible right now — no key material, a remote key
    /// set that has never loaded, or an unavailable revocation store.
    ProviderUnavailable,
    /// The auth configuration itself is invalid.
    Misconfigured,
}

impl AuthFailureReason {
    /// Every reason, in metric-slot order.
    pub const ALL: [Self; 14] = [
        Self::MissingCredentials,
        Self::MalformedToken,
        Self::AlgorithmRejected,
        Self::UnknownKey,
        Self::KeyRetired,
        Self::InvalidSignature,
        Self::Expired,
        Self::ClaimsRejected,
        Self::RefreshTokenRejected,
        Self::PolicyDenied,
        Self::Revoked,
        Self::CredentialMismatch,
        Self::ProviderUnavailable,
        Self::Misconfigured,
    ];

    /// The metric label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingCredentials => "missing_credentials",
            Self::MalformedToken => "malformed_token",
            Self::AlgorithmRejected => "algorithm_rejected",
            Self::UnknownKey => "unknown_key",
            Self::KeyRetired => "key_retired",
            Self::InvalidSignature => "invalid_signature",
            Self::Expired => "expired",
            Self::ClaimsRejected => "claims_rejected",
            Self::RefreshTokenRejected => "refresh_token_rejected",
            Self::PolicyDenied => "policy_denied",
            Self::Revoked => "revoked",
            Self::CredentialMismatch => "credential_mismatch",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::Misconfigured => "misconfigured",
        }
    }

    /// Index into `RuntimeState::auth_failure_reasons`.
    pub(crate) fn slot(self) -> usize {
        Self::ALL
            .iter()
            .position(|r| *r == self)
            .unwrap_or(Self::ALL.len() - 1)
    }

    /// When several providers or keys each failed a token, the most specific
    /// explanation wins: a token that verified but expired says more than a
    /// signature mismatch against some other provider's key.
    fn specificity(self) -> u8 {
        match self {
            Self::Expired | Self::ClaimsRejected => 4,
            Self::KeyRetired => 3,
            Self::InvalidSignature => 2,
            Self::UnknownKey => 1,
            _ => 0,
        }
    }
}

/// A failed authentication: the response status and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthFailure {
    /// The status the request is answered with — usually `401`; `503` when
    /// verification is impossible right now; `500` for a malformed route
    /// policy.
    pub status: StatusCode,
    /// The metric reason.
    pub reason: AuthFailureReason,
}

impl AuthFailure {
    fn new(status: StatusCode, reason: AuthFailureReason) -> Self {
        Self { status, reason }
    }

    fn unauthorized(reason: AuthFailureReason) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, reason)
    }
}

/// Authorization policy applied after a token verifies: required scopes and
/// roles, admin entitlement, tenant rules, route policies and required claims.
///
/// Built once per [`crate::http_runtime::RuntimeState`] from the environment.
/// `enforce_claim_policy` re-read six variables and re-parsed the route-policy
/// JSON on every authenticated request; the middleware now uses this snapshot.
#[derive(Debug, Clone)]
pub struct AuthPolicy {
    required_scopes: Vec<String>,
    required_roles: Vec<String>,
    admin_scope: String,
    admin_role: String,
    require_tenant_claim: bool,
    require_tenant_match: bool,
    /// `Err` when `KRAB_AUTH_ROUTE_POLICIES_JSON` is malformed: every request
    /// then fails closed with 500 rather than silently dropping the policies.
    route_policies: Result<Vec<RoutePolicy>, String>,
    /// `Err` when `KRAB_AUTH_REQUIRED_CLAIMS_JSON` is malformed (401, as before).
    required_claims: Result<std::collections::BTreeMap<String, Value>, String>,
}

impl AuthPolicy {
    /// Snapshot the policy variables: `KRAB_AUTH_REQUIRED_SCOPES` and
    /// `KRAB_AUTH_REQUIRED_ROLES` (comma-separated, all required),
    /// `KRAB_AUTH_ADMIN_SCOPE` / `KRAB_AUTH_ADMIN_ROLE` (default `admin`),
    /// `KRAB_AUTH_REQUIRE_TENANT_CLAIM` (default false),
    /// `KRAB_AUTH_REQUIRE_TENANT_MATCH` (default true),
    /// `KRAB_AUTH_ROUTE_POLICIES_JSON` and `KRAB_AUTH_REQUIRED_CLAIMS_JSON`.
    ///
    /// Never fails: malformed JSON is logged here and recorded, and every
    /// request is then rejected by [`AuthPolicy::enforce`] — `500` for route
    /// policies, `401` for required claims.
    pub fn from_env() -> Self {
        let csv = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| parse_csv_set(&v))
                .unwrap_or_default()
        };
        let route_policies = try_load_route_policies().map_err(|error| {
            tracing::error!(error = %error, "auth_route_policies_json_malformed");
            error.to_string()
        });
        let required_claims = match std::env::var("KRAB_AUTH_REQUIRED_CLAIMS_JSON") {
            Ok(raw) => serde_json::from_str(&raw).map_err(|error| {
                tracing::error!(error = %error, "auth_required_claims_json_malformed");
                error.to_string()
            }),
            Err(_) => Ok(std::collections::BTreeMap::new()),
        };
        Self {
            required_scopes: csv("KRAB_AUTH_REQUIRED_SCOPES"),
            required_roles: csv("KRAB_AUTH_REQUIRED_ROLES"),
            admin_scope: std::env::var("KRAB_AUTH_ADMIN_SCOPE")
                .unwrap_or_else(|_| "admin".to_string()),
            admin_role: std::env::var("KRAB_AUTH_ADMIN_ROLE")
                .unwrap_or_else(|_| "admin".to_string()),
            require_tenant_claim: crate::http::bool_env("KRAB_AUTH_REQUIRE_TENANT_CLAIM", false),
            require_tenant_match: crate::http::bool_env("KRAB_AUTH_REQUIRE_TENANT_MATCH", true),
            route_policies,
            required_claims,
        }
    }

    /// Whether `ctx` carries the admin scope or role.
    pub fn has_admin_entitlement(&self, ctx: &AuthContext) -> bool {
        ctx.scopes.iter().any(|s| s == &self.admin_scope)
            || ctx.roles.iter().any(|r| r == &self.admin_role)
    }

    /// Apply the policy to a verified token.
    pub fn enforce(
        &self,
        path: &str,
        claims: &JwtClaims,
        tenant_id: Option<&str>,
        scopes: &[String],
        roles: &[String],
    ) -> Result<(), StatusCode> {
        for scope in &self.required_scopes {
            if !scopes.iter().any(|s| s == scope) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }

        for role in &self.required_roles {
            if !roles.iter().any(|r| r == role) {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }

        if is_admin_api_path(path)
            && !scopes.iter().any(|s| s == &self.admin_scope)
            && !roles.iter().any(|r| r == &self.admin_role)
        {
            return Err(StatusCode::UNAUTHORIZED);
        }

        if self.require_tenant_claim && tenant_id.is_none() {
            return Err(StatusCode::UNAUTHORIZED);
        }

        if self.require_tenant_match {
            if let Some(path_tenant) = tenant_from_path(path) {
                if tenant_id != Some(path_tenant) {
                    return Err(StatusCode::UNAUTHORIZED);
                }
            }
        }

        // Fail closed on malformed policy JSON: an empty policy set here would
        // silently drop every configured restriction.
        let route_policies = self.route_policies.as_ref().map_err(|error| {
            tracing::error!(
                error = %error,
                "auth_route_policies_json_malformed_failing_closed"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        for policy in route_policies
            .iter()
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

        let required = self
            .required_claims
            .as_ref()
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        if !required.is_empty() {
            let claims_json = serde_json::to_value(claims).map_err(|_| StatusCode::UNAUTHORIZED)?;
            let claims_obj = claims_json.as_object().ok_or(StatusCode::UNAUTHORIZED)?;
            for (k, v) in required {
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
}

/// Apply the claim policy configured in the environment.
///
/// Reads and parses the environment on every call; the auth middleware uses
/// the [`AuthPolicy`] snapshot held on `RuntimeState` instead.
pub fn enforce_claim_policy(
    path: &str,
    claims: &JwtClaims,
    tenant_id: Option<&str>,
    scopes: &[String],
    roles: &[String],
) -> Result<(), StatusCode> {
    AuthPolicy::from_env().enforce(path, claims, tenant_id, scopes, roles)
}

/// Static-bearer authentication (`KRAB_AUTH_MODE=static`, dev only).
///
/// The `Authorization` header must equal `Bearer {KRAB_BEARER_TOKEN}`,
/// compared in constant time; the token is read through
/// [`read_env_or_file`](crate::config::read_env_or_file) on every call.
/// `401` when the header is missing or differs, `503` when no token is
/// configured. On success the [`AuthContext`] is the fixed static identity.
pub fn authorize_with_static_bearer(req: &Request<Body>) -> Result<AuthContext, StatusCode> {
    authorize_static_detailed(req).map_err(|failure| failure.status)
}

fn authorize_static_detailed(req: &Request<Body>) -> Result<AuthContext, AuthFailure> {
    // Through `read_env_or_file`, like every other secret: `KRAB_BEARER_TOKEN`
    // was read with `std::env::var`, so the `_FILE` and `_VAULT_REF` forms the
    // configuration docs promise for secrets were silently ignored for it.
    let unavailable = AuthFailure::new(
        StatusCode::SERVICE_UNAVAILABLE,
        AuthFailureReason::ProviderUnavailable,
    );
    let expected = match crate::config::read_env_or_file("KRAB_BEARER_TOKEN") {
        Ok(Some(token)) if !token.trim().is_empty() => token,
        Ok(_) => {
            tracing::warn!("KRAB_BEARER_TOKEN is not configured for static auth mode");
            return Err(unavailable);
        }
        Err(error) => {
            tracing::warn!(%error, "KRAB_BEARER_TOKEN could not be read for static auth mode");
            return Err(unavailable);
        }
    };
    let expected = format!("Bearer {expected}");

    let Some(presented) = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
    else {
        return Err(AuthFailure::unauthorized(
            AuthFailureReason::MissingCredentials,
        ));
    };

    if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return Err(AuthFailure::unauthorized(
            AuthFailureReason::CredentialMismatch,
        ));
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
/// request. Remote key sets are not fetched on this path.
pub fn authorize_with_jwt(req: &Request<Body>, path: &str) -> Result<AuthContext, StatusCode> {
    let cache = JwtVerifierCache::from_env();
    authorize_with_jwt_cached(req, path, &cache)
}

/// Verify the request's bearer JWT against `cache` and apply the claim
/// policy, returning the caller's [`AuthContext`] or the status to answer
/// with.
///
/// Checks, in order: a `Bearer` token is present and decodes; its `alg` is
/// allowed ([`jwt_algorithm_allowed`]); some provider has a non-retired key
/// for its `kid` under which the signature and `exp` verify and
/// [`validate_provider_claims`] passes; it is not a refresh token; and the
/// [`AuthPolicy`] allows it. The policy is read from the environment on each
/// call; remote key sets are used as already loaded, never fetched. Unlike
/// [`auth_middleware`], this does not consult the revocation list.
pub fn authorize_with_jwt_cached(
    req: &Request<Body>,
    path: &str,
    cache: &JwtVerifierCache,
) -> Result<AuthContext, StatusCode> {
    authorize_jwt_detailed(req, path, cache, &AuthPolicy::from_env())
        .map_err(|failure| failure.status)
}

/// The bearer token of `req`, if it carries one.
fn bearer_token(req: &Request<Body>) -> Option<&str> {
    req.headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn authorize_jwt_detailed(
    req: &Request<Body>,
    path: &str,
    cache: &JwtVerifierCache,
    policy: &AuthPolicy,
) -> Result<AuthContext, AuthFailure> {
    let bearer = bearer_token(req).ok_or(AuthFailure::unauthorized(
        AuthFailureReason::MissingCredentials,
    ))?;

    let header = jsonwebtoken::decode_header(bearer)
        .map_err(|_| AuthFailure::unauthorized(AuthFailureReason::MalformedToken))?;

    let env = crate::config::Environment::from_env();
    let alg = format!("{:?}", header.alg);
    let allowed_algs = configured_jwt_algorithms()
        .map_err(|status| AuthFailure::new(status, AuthFailureReason::Misconfigured))?;
    if !allowed_algs.contains(&header.alg) || !jwt_algorithm_allowed(&alg, &env) {
        warn!(
            alg = ?header.alg,
            environment = %env.as_str(),
            "jwt_algorithm_rejected_by_hardening_profile"
        );
        return Err(AuthFailure::unauthorized(
            AuthFailureReason::AlgorithmRejected,
        ));
    }

    if cache.load_failed() {
        warn!("jwt_provider_configuration_unavailable_rejecting_request");
        return Err(AuthFailure::new(
            StatusCode::SERVICE_UNAVAILABLE,
            AuthFailureReason::Misconfigured,
        ));
    }

    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.algorithms = allowed_algs.clone();
    validation.validate_exp = true;
    validation.validate_aud = false;
    validation.leeway = jwt_leeway_secs();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX);

    let mut failure = AuthFailureReason::UnknownKey;
    let mut note = |reason: AuthFailureReason| {
        if reason.specificity() > failure.specificity() {
            failure = reason;
        }
    };
    let mut remote_unavailable = false;
    let mut kidless_trials = 0usize;

    let mut accepted: Option<(JwtClaims, String)> = None;
    'providers: for (provider_index, provider) in cache.providers().iter().enumerate() {
        let kids = cache.candidate_kids(provider_index, header.kid.as_deref());
        if kids.is_empty() && cache.remote_unavailable(provider_index) {
            remote_unavailable = true;
        }
        for selected_kid in &kids {
            if cache.is_retired(provider_index, selected_kid, now) {
                warn!(
                    provider = provider.name.as_deref().unwrap_or("provider"),
                    kid = %selected_kid,
                    "jwt_key_retired_rejecting_token"
                );
                note(AuthFailureReason::KeyRetired);
                continue;
            }
            let Some(decoding_key) = cache.decoding_key(provider_index, selected_kid, header.alg)
            else {
                warn!(
                    provider = provider.name.as_deref().unwrap_or("provider"),
                    alg = ?header.alg,
                    "jwt_provider_key_material_invalid_for_algorithm"
                );
                continue;
            };

            if header.kid.is_none() {
                if kidless_trials >= MAX_KIDLESS_KEY_TRIALS {
                    warn!(
                        max_trials = MAX_KIDLESS_KEY_TRIALS,
                        "jwt_kidless_key_trial_cap_reached"
                    );
                    break 'providers;
                }
                kidless_trials += 1;
            }

            let token_data =
                match jsonwebtoken::decode::<JwtClaims>(bearer, &decoding_key, &validation) {
                    Ok(data) => data,
                    Err(error) => {
                        note(match error.kind() {
                            jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                                AuthFailureReason::Expired
                            }
                            jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                                AuthFailureReason::InvalidSignature
                            }
                            _ => AuthFailureReason::MalformedToken,
                        });
                        continue;
                    }
                };

            if validate_provider_claims(&token_data.claims, provider).is_err() {
                note(AuthFailureReason::ClaimsRejected);
                continue;
            }

            accepted = Some((
                token_data.claims,
                provider
                    .name
                    .clone()
                    .unwrap_or_else(|| "provider".to_string()),
            ));
            break 'providers;
        }
    }

    let Some((claims, provider_name)) = accepted else {
        if failure == AuthFailureReason::UnknownKey && remote_unavailable {
            // The only key sets that could have held this token's key never
            // loaded: an outage on our side, not a bad token.
            return Err(AuthFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                AuthFailureReason::ProviderUnavailable,
            ));
        }
        return Err(AuthFailure::unauthorized(failure));
    };
    if matches!(claims.token_use.as_deref(), Some("refresh")) {
        warn!("refresh_token_presented_to_access_protected_route");
        return Err(AuthFailure::unauthorized(
            AuthFailureReason::RefreshTokenRejected,
        ));
    }

    let scopes = scopes_from_claims(&claims);
    let roles = roles_from_claims(&claims);
    let tenant_id = tenant_from_claims(&claims);
    policy
        .enforce(path, &claims, tenant_id.as_deref(), &scopes, &roles)
        .map_err(|status| {
            let reason = if status == StatusCode::INTERNAL_SERVER_ERROR {
                AuthFailureReason::Misconfigured
            } else {
                AuthFailureReason::PolicyDenied
            };
            AuthFailure::new(status, reason)
        })?;

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

/// Framework-owned unauthenticated paths: the probes every Krab service
/// serves. Always part of the default list.
pub(crate) const FRAMEWORK_OPEN_PATHS: &[&str] = &["/health", "/ready"];

/// Application routes that shipped on the framework's default open list —
/// the reference frontend's and auth service's public pages. A framework
/// default that opens `/contact` or `/rpc/now` in every consumer is a policy
/// decision made on the consumer's behalf.
///
/// **Deprecated in 0.6.0, removed from the default in 0.7.0.** Until then
/// they stay open when `KRAB_AUTH_OPEN_PATHS` is unset, a startup warning
/// names them, and `KRAB_AUTH_LEGACY_OPEN_PATHS=false` drops them early.
/// Services declare their public routes with
/// [`RuntimeState::with_public_paths`](crate::http_runtime::RuntimeState::with_public_paths)
/// or `KRAB_AUTH_PUBLIC_PATHS`.
pub(crate) const LEGACY_APP_OPEN_PATHS: &[&str] = &[
    "/",
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
/// path), otherwise [`FRAMEWORK_OPEN_PATHS`] plus — unless
/// `KRAB_AUTH_LEGACY_OPEN_PATHS=false` — the deprecated
/// [`LEGACY_APP_OPEN_PATHS`]. [`METRICS_OPEN_PATHS`] is appended in either
/// case when `KRAB_METRICS_PUBLIC` is on.
pub(crate) fn auth_open_path_patterns() -> Vec<String> {
    let mut patterns: Vec<String> = match std::env::var("KRAB_AUTH_OPEN_PATHS") {
        Ok(raw) => parse_csv_set(&raw),
        Err(_) => {
            let mut defaults: Vec<String> =
                FRAMEWORK_OPEN_PATHS.iter().map(|s| s.to_string()).collect();
            if crate::http::bool_env("KRAB_AUTH_LEGACY_OPEN_PATHS", true) {
                warn!(
                    paths = ?LEGACY_APP_OPEN_PATHS,
                    removed_in = "0.7.0",
                    "auth_legacy_default_open_paths_in_use: these application routes are \
                     unauthenticated only because they are on the framework's default list; \
                     declare the ones this service serves with KRAB_AUTH_PUBLIC_PATHS or \
                     RuntimeState::with_public_paths, then set KRAB_AUTH_LEGACY_OPEN_PATHS=false"
                );
                defaults.extend(LEGACY_APP_OPEN_PATHS.iter().map(|s| s.to_string()));
            }
            defaults
        }
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

/// Authenticates every request that is not on an open or public path.
///
/// Open paths are `KRAB_AUTH_OPEN_PATHS` or the defaults (plus the metrics
/// paths when `KRAB_METRICS_PUBLIC` is on); public paths are those of
/// [`RuntimeState::is_public_path`](crate::http_runtime::RuntimeState::is_public_path)
/// — `KRAB_AUTH_PUBLIC_PATHS`, plus code-declared paths only while
/// `KRAB_AUTH_OPEN_PATHS` is unset.
/// Anything else is verified per `KRAB_AUTH_MODE` — JWT/OIDC through the
/// state's verifier cache and policy (fetching a remote key set first if the
/// token's `kid` needs it), or the static bearer token. A token whose `jti`
/// is on the revocation list (`auth:revoked:{jti}` in the runtime store) is
/// rejected; if the store cannot be read, the request fails closed with
/// `503`. `/api/admin` and `/api/v{n}/admin` paths also require the admin
/// entitlement.
///
/// On success the [`AuthContext`] is inserted into the request extensions.
/// On failure the reason is counted; a `401` is also counted against the
/// client IP's window: past `auth_fail_threshold` failures the answer
/// becomes `429`, and if that counter's store is unavailable, `503`. Our own
/// outages (`503`, `500`) are not counted against the client.
///
/// The limiter is consulted *before* verification only for a bearer token
/// whose JWT header has **no `kid`** (it may be tried against several keys, so
/// a flood of them is a CPU amplifier): from an address already past the
/// threshold such a token is answered `429` unverified, valid or not, until the
/// window rolls over. Every other request — a JWT that names a `kid`, a static
/// token, a missing header — is verified first; its failures still count and
/// can still earn `429`, but a valid token from an over-budget address (a
/// shared NAT, say) authenticates. A store read error skips the pre-check.
pub async fn auth_middleware<S>(
    State(state): State<S>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode>
where
    S: Clone + Send + Sync + 'static + HasRuntimeState,
{
    let path = req.uri().path().to_string();
    let runtime = state.runtime_state();

    let open = path_matches_patterns(&path, &runtime.auth_open_paths);
    let is_public = runtime.is_public_path(&path);

    if open || is_public {
        return Ok(next.run(req).await);
    }

    let client_ip = extract_client_ip(&req, runtime.trust_proxy_headers);
    let auth_window_secs = runtime.auth_fail_window_secs;
    let auth_window = current_window_epoch(auth_window_secs);
    let auth_key = format!("auth:fail:{client_ip}:{auth_window}");

    // For a token WITHOUT a `kid`, consult the failure budget before
    // verifying: such a token is tried against up to MAX_KIDLESS_KEY_TRIALS
    // keys, so a flood of them from one address is a CPU amplifier, and an
    // address already over budget is answered 429 without spending those
    // verifications. Tokens naming a `kid` cost one verification (as in
    // 0.5.0) and skip this check, for two reasons: it would add a store round
    // trip to every authenticated request, and it would reject *valid* tokens
    // from an address over budget — letting anyone behind the same NAT lock
    // real users out. A store read error skips the check; a failure that
    // follows is still counted, and fails closed, below.
    let kidless_bearer = bearer_token(&req)
        .and_then(|token| jsonwebtoken::decode_header(token).ok())
        .is_some_and(|header| header.kid.is_none());
    let pre_check = if kidless_bearer {
        runtime.store.get(&auth_key).await
    } else {
        Ok(None)
    };
    match pre_check {
        Ok(Some(count))
            if count
                .trim()
                .parse::<u64>()
                .is_ok_and(|count| count > runtime.auth_fail_threshold) =>
        {
            debug!(
                client_ip = %client_ip,
                threshold = runtime.auth_fail_threshold,
                "auth_failure_rate_limiter_rejected_before_verification"
            );
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        Ok(_) => {}
        Err(err) => {
            debug!(
                error = %err,
                client_ip = %client_ip,
                "auth_failure_budget_lookup_failed_verifying_anyway"
            );
        }
    }

    let mode = runtime.auth_mode.clone();
    let authorized = if mode.eq_ignore_ascii_case("jwt") || mode.eq_ignore_ascii_case("oidc") {
        let cache = &runtime.jwt_verifier_cache;
        if cache.uses_jwks() {
            // Off the synchronous verify path: fetch a key set that has not
            // loaded yet, or refetch one missing this token's `kid` (a key
            // rotation at the provider), within the refetch rate limit.
            let kid = bearer_token(&req)
                .and_then(|token| jsonwebtoken::decode_header(token).ok())
                .and_then(|header| header.kid);
            cache.prepare_for_kid(kid.as_deref()).await;
        }
        authorize_jwt_detailed(&req, &path, cache, &runtime.auth_policy)
    } else {
        authorize_static_detailed(&req)
    };
    let authorized = match authorized {
        Ok(ctx) => {
            if let Some(token_id) = ctx.token_id.as_deref() {
                let revoked_key = format!("auth:revoked:{token_id}");
                match runtime.store.get(&revoked_key).await {
                    Ok(Some(_)) => {
                        warn!(token_id = %token_id, path = %path, "revoked_token_rejected");
                        Err(AuthFailure::unauthorized(AuthFailureReason::Revoked))
                    }
                    Ok(None) => Ok(ctx),
                    Err(err) => {
                        warn!(
                            error = %err,
                            token_id = %token_id,
                            "revocation_lookup_failed_failing_closed"
                        );
                        Err(AuthFailure::new(
                            StatusCode::SERVICE_UNAVAILABLE,
                            AuthFailureReason::ProviderUnavailable,
                        ))
                    }
                }
            } else {
                Ok(ctx)
            }
        }
        Err(failure) => Err(failure),
    };

    match authorized {
        Ok(ctx) => {
            if is_admin_api_path(&path) && !runtime.auth_policy.has_admin_entitlement(&ctx) {
                return Err(StatusCode::FORBIDDEN);
            }
            req.extensions_mut().insert(ctx);
            Ok(next.run(req).await)
        }
        Err(AuthFailure {
            status: code,
            reason,
        }) => {
            runtime.auth_failures_total.fetch_add(1, Ordering::Relaxed);
            runtime.auth_failure_reasons[reason.slot()].fetch_add(1, Ordering::Relaxed);
            debug!(
                reason = reason.as_str(),
                status = code.as_u16(),
                "auth_request_rejected"
            );

            // Only the caller's failures count against the caller's budget.
            // A 503 (key set or revocation store unavailable) or 500
            // (misconfiguration) is our outage: counting it turned an IdP
            // outage into 429s for every client once it outlasted the
            // threshold.
            if code != StatusCode::UNAUTHORIZED {
                return Err(code);
            }

            let failures = match runtime
                .store
                .incr_with_ttl(&auth_key, 1, Duration::from_secs(auth_window_secs + 2))
                .await
            {
                Ok(count) => count,
                Err(err) => {
                    // Fail closed, but say what happened: the store is down,
                    // which is a 503. This returned 429, telling a client it
                    // was being rate limited — and telling an operator's
                    // dashboards the same — during what was an outage.
                    warn!(
                        error = %err,
                        client_ip = %client_ip,
                        "auth_failure_store_error_failing_closed"
                    );
                    return Err(StatusCode::SERVICE_UNAVAILABLE);
                }
            };

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

/// Whether `path` is a service-to-service route: it starts with `/internal`
/// or `/api/internal` (a plain string prefix, so `/internals` matches too).
pub fn is_internal_service_path(path: &str) -> bool {
    path.starts_with("/internal") || path.starts_with("/api/internal")
}

/// Guards [internal service paths](is_internal_service_path): the request's
/// [`AuthContext`] must carry the runtime's `service_auth_scope`
/// (`KRAB_SERVICE_AUTH_SCOPE`), or it is answered `403`. Other paths pass
/// through. Must run after [`auth_middleware`], which supplies the context.
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
    use super::{
        auth_open_path_patterns, path_matches_patterns, FRAMEWORK_OPEN_PATHS, LEGACY_APP_OPEN_PATHS,
    };
    use serial_test::serial;

    /// Every test here resolves the pattern list from the environment, so the
    /// knobs must start unset regardless of what ran before.
    fn reset_open_path_env() {
        std::env::remove_var("KRAB_AUTH_OPEN_PATHS");
        std::env::remove_var("KRAB_METRICS_PUBLIC");
        std::env::remove_var("KRAB_AUTH_LEGACY_OPEN_PATHS");
    }

    /// Opting out of the deprecated application routes leaves only the
    /// framework's own probes open — the 0.7.0 default.
    #[test]
    #[serial]
    fn legacy_app_paths_can_be_dropped_ahead_of_0_7() {
        reset_open_path_env();
        std::env::set_var("KRAB_AUTH_LEGACY_OPEN_PATHS", "false");
        let patterns = auth_open_path_patterns();
        reset_open_path_env();

        assert_eq!(
            patterns,
            FRAMEWORK_OPEN_PATHS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert!(!path_matches_patterns("/", &patterns));
        assert!(!path_matches_patterns("/contact", &patterns));
        assert!(path_matches_patterns("/ready", &patterns));
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

        // 0.6.x default: framework probes plus the deprecated app routes.
        assert_eq!(
            patterns,
            FRAMEWORK_OPEN_PATHS
                .iter()
                .chain(LEGACY_APP_OPEN_PATHS)
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

#[cfg(test)]
mod jwks_lazy_refresh_tests {
    use super::{JwtProviderConfig, JwtVerifierCache};
    use serde_json::{json, Value};
    use serial_test::serial;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Base64url `x` of a throwaway Ed25519 public key (the auth suite's
    /// `ED_X_1`).
    const ED_X: &str = "tQCGXC6DpH3eQ7mQpTmUwz_UrjPnQ-X2ztczWt5Uyis";

    fn jwk(kid: &str) -> Value {
        json!({"kty": "OKP", "crv": "Ed25519", "kid": kid, "x": ED_X, "use": "sig"})
    }

    /// Built outside a Tokio runtime, the cache cannot schedule a background
    /// refresh. It used to return silently, and a key removed at the
    /// provider then kept verifying for the life of the process; now the
    /// request path refreshes a set older than the refresh interval.
    #[test]
    #[serial]
    fn without_a_runtime_key_sets_are_refreshed_on_the_request_path() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let document = Arc::new(Mutex::new(json!({"keys": [jwk("k1")]})));
        let served = document.clone();
        let url = runtime.block_on(async move {
            let app = axum::Router::new().route(
                "/jwks",
                axum::routing::get(move || {
                    let served = served.clone();
                    async move { axum::Json(served.lock().unwrap().clone()) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            format!("http://{addr}/jwks")
        });

        std::env::remove_var("KRAB_ENVIRONMENT");
        let provider = JwtProviderConfig {
            name: Some("idp".to_string()),
            issuer: None,
            audience: None,
            keys: Default::default(),
            required_claims: Default::default(),
            jwks_url: Some(url),
            key_not_after: Default::default(),
        };
        let cache = Arc::new(JwtVerifierCache::from_providers_with(
            vec![provider],
            crate::jwks::JwksSettings {
                refresh_every: Duration::from_millis(200),
                min_refetch: Duration::from_millis(50),
                timeout: Duration::from_secs(2),
            },
        ));
        // Not inside the runtime: nothing can be spawned.
        JwtVerifierCache::spawn_background_refresh(&cache);
        let source = cache.jwks[0].clone().unwrap();

        runtime.block_on(cache.prepare_for_kid(Some("k1")));
        assert!(source.has_kid("k1"));

        // The provider retires k1.
        *document.lock().unwrap() = json!({"keys": [jwk("k2")]});
        std::thread::sleep(Duration::from_millis(300));
        runtime.block_on(cache.prepare_for_kid(Some("k1")));

        assert!(
            !source.has_kid("k1"),
            "a key removed at the provider must stop verifying without a background task"
        );
        assert!(source.has_kid("k2"));
    }
}
