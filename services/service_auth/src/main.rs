use anyhow::{Context as _, Result};
use async_trait::async_trait;
use axum::routing::{get, post};
use axum::{Json, Router};
use jsonwebtoken::{
    decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use krab_core::config::KrabConfig;
use krab_core::config::{env_non_empty, read_env_or_file};
use krab_core::credentials::{
    hash_password, is_valid_password_hash, CredentialStore as _, EnvHashCredentialStore,
};
use krab_core::http::{
    apply_common_http_layers, health, metrics, metrics_prometheus, readiness_with_dependencies,
    HasReadinessDependencies, HasRuntimeState, RuntimeState,
};
use krab_core::protocol::{ProtocolConfig, ProtocolKind};
use krab_core::service::{serve_with_graceful_shutdown, ApiService, ServiceConfig};
use krab_core::telemetry::init_tracing;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use uuid::Uuid;

const INSECURE_DEV_JWT_SECRET: &str = "krab-insecure-dev-secret-change-me";
const INSECURE_DEV_BOOTSTRAP_PASSWORD: &str = "change-me";

struct AuthService {
    config: ServiceConfig,
    credentials: Arc<EnvHashCredentialStore>,
}

#[derive(Clone)]
struct AppState {
    runtime: RuntimeState,
    /// Built once at startup. Argon2 verification is deliberately expensive, so
    /// re-parsing the credential map per request would add that cost to every
    /// login on top of the hashing itself.
    credentials: Arc<EnvHashCredentialStore>,
}

#[derive(Clone)]
struct SigningConfig {
    issuer: String,
    audience: String,
    access_ttl_secs: u64,
    refresh_ttl_secs: u64,
}

#[derive(Clone)]
struct KeyRing {
    active_kid: String,
    keys: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AuthTokenClaims {
    sub: String,
    iss: String,
    aud: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    jti: String,
    token_use: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
    scope: String,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Debug, Deserialize)]
struct RevokeRequest {
    token: String,
}

#[derive(Debug, Serialize)]
struct TokenPair {
    token_type: &'static str,
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    refresh_expires_in: u64,
    kid: String,
}

#[derive(Debug, Serialize)]
struct JwkDescriptor {
    kty: &'static str,
    kid: String,
    alg: &'static str,
    r#use: &'static str,
}

impl HasReadinessDependencies for AppState {
    fn readiness_dependencies(&self) -> Vec<krab_core::http::DependencyStatus> {
        vec![krab_core::http::DependencyStatus {
            name: "auth_runtime",
            ready: true,
            critical: true,
            latency_ms: Some(0),
            detail: Some("runtime-initialized".to_string()),
        }]
    }
}

async fn root() -> &'static str {
    "Auth Service Online"
}

async fn private() -> &'static str {
    "private_ok"
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ttl_from_exp(exp: i64) -> Duration {
    let now = now_epoch_secs();
    let secs = if exp <= now { 1 } else { (exp - now) as u64 };
    Duration::from_secs(secs)
}

fn signing_config_from_env() -> SigningConfig {
    SigningConfig {
        issuer: std::env::var("KRAB_OIDC_ISSUER").unwrap_or_else(|_| "krab.auth".to_string()),
        audience: std::env::var("KRAB_OIDC_AUDIENCE")
            .unwrap_or_else(|_| "krab.services".to_string()),
        access_ttl_secs: std::env::var("KRAB_AUTH_ACCESS_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(900),
        refresh_ttl_secs: std::env::var("KRAB_AUTH_REFRESH_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60 * 60 * 24 * 7),
    }
}

fn key_ring_from_env() -> Result<KeyRing> {
    let keys = if let Some(raw) = read_env_or_file("KRAB_JWT_KEYS_JSON")? {
        serde_json::from_str::<BTreeMap<String, String>>(&raw)
            .context("KRAB_JWT_KEYS_JSON must be a JSON object of kid->secret")?
    } else {
        let secret = read_env_or_file("KRAB_JWT_SECRET")?
            .unwrap_or_else(|| INSECURE_DEV_JWT_SECRET.to_string());
        let mut map = BTreeMap::new();
        map.insert("default".to_string(), secret);
        map
    };

    anyhow::ensure!(!keys.is_empty(), "no JWT signing keys are configured");
    let active_kid = std::env::var("KRAB_JWT_ACTIVE_KID")
        .ok()
        .filter(|v| keys.contains_key(v))
        .or_else(|| keys.keys().next().cloned())
        .context("failed to resolve active JWT signing key id")?;

    Ok(KeyRing { active_kid, keys })
}

/// Whether `KRAB_ENVIRONMENT` names an environment where developer conveniences
/// — an unhashed bootstrap password, a default JWT secret — are tolerated.
///
/// Shared by the startup guard and [`resolve_credential_store`] so the two
/// cannot drift into disagreeing about what counts as production.
fn local_like_environment() -> bool {
    let env = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
    env.eq_ignore_ascii_case("dev") || env.eq_ignore_ascii_case("local")
}

/// Reject any configured credential that is not an Argon2id PHC hash.
///
/// Runs outside `local`-like environments only, and covers `*_FILE` and
/// `*_VAULT_REF` sourcing as well as inline values — a plaintext password read
/// from a mounted secret is still a plaintext password.
fn enforce_hashed_credentials() -> Result<()> {
    if let Some(raw) = read_env_or_file("KRAB_AUTH_LOGIN_USERS_JSON")? {
        let configured = serde_json::from_str::<BTreeMap<String, String>>(&raw).context(
            "KRAB_AUTH_LOGIN_USERS_JSON must be a JSON object of username -> Argon2id PHC hash",
        )?;
        for (username, value) in &configured {
            anyhow::ensure!(
                is_valid_password_hash(value),
                "KRAB_AUTH_LOGIN_USERS_JSON entry for '{username}' is not an Argon2id PHC hash; \
                 plaintext credentials are forbidden outside local environments. \
                 Generate one with `krab auth hash-password`"
            );
        }
    }

    if let Some(password) = read_env_or_file("KRAB_AUTH_BOOTSTRAP_PASSWORD")? {
        anyhow::ensure!(
            is_valid_password_hash(&password),
            "KRAB_AUTH_BOOTSTRAP_PASSWORD is not an Argon2id PHC hash; plaintext credentials \
             are forbidden outside local environments. Generate one with `krab auth hash-password`"
        );
    }

    Ok(())
}

/// Build the credential store from the environment, once, at startup.
///
/// `KRAB_AUTH_LOGIN_USERS_JSON` is a JSON object of `username -> Argon2id PHC
/// hash`. It previously held plaintext passwords; see the migration guide.
///
/// In `local`-like environments a value that is not a PHC hash is accepted and
/// hashed here, so `KRAB_AUTH_BOOTSTRAP_PASSWORD=change-me` still works for
/// development without a hashing step. That conversion happens exactly once at
/// startup, so no plaintext comparison exists on the request path and no
/// Argon2 cost is paid per login. Outside `local` the startup guard rejects
/// non-PHC values outright — see [`enforce_hashed_credentials`].
fn resolve_credential_store() -> Result<EnvHashCredentialStore> {
    let allow_plaintext = local_like_environment();
    let mut users: BTreeMap<String, String> = BTreeMap::new();

    if let Some(raw) = read_env_or_file("KRAB_AUTH_LOGIN_USERS_JSON")? {
        let configured = serde_json::from_str::<BTreeMap<String, String>>(&raw).context(
            "KRAB_AUTH_LOGIN_USERS_JSON must be a JSON object of username -> Argon2id PHC hash",
        )?;
        for (username, value) in configured {
            users.insert(username, normalize_credential(&value, allow_plaintext)?);
        }
    }

    let default_user =
        std::env::var("KRAB_AUTH_BOOTSTRAP_USER").unwrap_or_else(|_| "admin".to_string());
    if let std::collections::btree_map::Entry::Vacant(entry) = users.entry(default_user) {
        let configured = read_env_or_file("KRAB_AUTH_BOOTSTRAP_PASSWORD")?
            .unwrap_or_else(|| INSECURE_DEV_BOOTSTRAP_PASSWORD.to_string());
        entry.insert(normalize_credential(&configured, allow_plaintext)?);
    }

    EnvHashCredentialStore::from_map(users)
}

/// Pass a PHC hash through unchanged; hash a plaintext value only where that is
/// permitted.
fn normalize_credential(value: &str, allow_plaintext: bool) -> Result<String> {
    if is_valid_password_hash(value) {
        return Ok(value.to_string());
    }

    anyhow::ensure!(
        allow_plaintext,
        "credential is not an Argon2id PHC hash; generate one with `krab auth hash-password`"
    );

    warn!(
        event = "credential_plaintext_hashed_at_startup",
        "a plaintext credential was hashed at startup; this is permitted only in local environments"
    );
    hash_password(value)
}

fn encode_hs256(kid: &str, secret: &str, claims: &AuthTokenClaims) -> Result<String> {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_string());
    encode(
        &header,
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("failed to encode JWT")
}

fn decode_with_key_ring(
    token: &str,
    key_ring: &KeyRing,
    cfg: &SigningConfig,
) -> Result<AuthTokenClaims, axum::http::StatusCode> {
    let header = decode_header(token).map_err(|_| axum::http::StatusCode::UNAUTHORIZED)?;
    let candidate_keys: Vec<(&String, &String)> = match header.kid.as_deref() {
        Some(kid) => key_ring
            .keys
            .get_key_value(kid)
            .into_iter()
            .collect::<Vec<(&String, &String)>>(),
        None => key_ring.keys.iter().collect::<Vec<(&String, &String)>>(),
    };

    for (_kid, secret) in candidate_keys {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[cfg.audience.as_str()]);
        validation.set_issuer(&[cfg.issuer.as_str()]);
        validation.leeway = 30;
        if let Ok(data) = decode::<AuthTokenClaims>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        ) {
            return Ok(data.claims);
        }
    }

    Err(axum::http::StatusCode::UNAUTHORIZED)
}

async fn issue_token_pair(
    runtime: &RuntimeState,
    subject: &str,
    tenant_id: Option<String>,
    scopes: Vec<String>,
    roles: Vec<String>,
) -> Result<TokenPair> {
    let cfg = signing_config_from_env();
    let key_ring = key_ring_from_env()?;
    let secret = key_ring
        .keys
        .get(&key_ring.active_kid)
        .context("active signing key not found")?;

    let now = now_epoch_secs();
    let access_jti = Uuid::new_v4().to_string();
    let refresh_jti = Uuid::new_v4().to_string();
    let scope = if scopes.is_empty() {
        "user".to_string()
    } else {
        scopes.join(" ")
    };
    let roles = if roles.is_empty() {
        vec!["user".to_string()]
    } else {
        roles
    };

    let access_claims = AuthTokenClaims {
        sub: subject.to_string(),
        iss: cfg.issuer.clone(),
        aud: cfg.audience.clone(),
        iat: now,
        nbf: now,
        exp: now + cfg.access_ttl_secs as i64,
        jti: access_jti,
        token_use: "access".to_string(),
        tenant_id: tenant_id.clone(),
        scope: scope.clone(),
        roles: roles.clone(),
    };
    let refresh_claims = AuthTokenClaims {
        sub: subject.to_string(),
        iss: cfg.issuer.clone(),
        aud: cfg.audience.clone(),
        iat: now,
        nbf: now,
        exp: now + cfg.refresh_ttl_secs as i64,
        jti: refresh_jti.clone(),
        token_use: "refresh".to_string(),
        tenant_id,
        scope,
        roles,
    };

    let access_token = encode_hs256(&key_ring.active_kid, secret, &access_claims)?;
    let refresh_token = encode_hs256(&key_ring.active_kid, secret, &refresh_claims)?;

    let refresh_ttl = Duration::from_secs(cfg.refresh_ttl_secs.max(1));
    runtime
        .store
        .set(
            &format!("auth:refresh:live:{}", refresh_jti),
            "1",
            refresh_ttl,
        )
        .await
        .map_err(|e| anyhow::anyhow!("failed to persist refresh token: {e}"))?;

    Ok(TokenPair {
        token_type: "Bearer",
        access_token,
        refresh_token,
        expires_in: cfg.access_ttl_secs,
        refresh_expires_in: cfg.refresh_ttl_secs,
        kid: key_ring.active_kid,
    })
}

async fn login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    // Argon2id verification against a stored PHC hash. An unknown username
    // costs the same as a wrong password — see `EnvHashCredentialStore::verify`.
    match state
        .credentials
        .verify(payload.username.trim(), payload.password.trim())
        .await
    {
        Ok(Some(_identity)) => {}
        Ok(None) => {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({"error":"invalid_credentials"})),
            );
        }
        Err(err) => {
            // A verification fault is a misconfiguration, not a failed login;
            // the detail never contains the submitted password.
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"auth_config_error","detail":err.to_string()})),
            );
        }
    }

    match issue_token_pair(
        &state.runtime,
        payload.username.trim(),
        payload.tenant_id,
        payload.scopes,
        payload.roles,
    )
    .await
    {
        Ok(pair) => (axum::http::StatusCode::OK, Json(json!(pair))),
        Err(err) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"token_issuance_failed","detail":err.to_string()})),
        ),
    }
}

async fn refresh_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<RefreshRequest>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let cfg = signing_config_from_env();
    let Ok(key_ring) = key_ring_from_env() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"signing_keys_unavailable"})),
        );
    };

    let Ok(claims) = decode_with_key_ring(&payload.refresh_token, &key_ring, &cfg) else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid_refresh_token"})),
        );
    };

    if claims.token_use != "refresh" {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"token_use_mismatch"})),
        );
    }

    let used_key = format!("auth:refresh:used:{}", claims.jti);
    if state
        .runtime
        .store
        .get(&used_key)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"refresh_token_already_used"})),
        );
    }

    let revoked_key = format!("auth:revoked:{}", claims.jti);
    if state
        .runtime
        .store
        .get(&revoked_key)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"refresh_token_revoked"})),
        );
    }

    let ttl = ttl_from_exp(claims.exp);
    if let Err(err) = state.runtime.store.set(&used_key, "1", ttl).await {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"store_unavailable","detail":err.to_string()})),
        );
    }

    let scopes = claims
        .scope
        .split_whitespace()
        .map(|v| v.to_string())
        .collect::<Vec<String>>();
    match issue_token_pair(
        &state.runtime,
        &claims.sub,
        claims.tenant_id,
        scopes,
        claims.roles,
    )
    .await
    {
        Ok(pair) => (axum::http::StatusCode::OK, Json(json!(pair))),
        Err(err) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"refresh_failed","detail":err.to_string()})),
        ),
    }
}

async fn revoke_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    Json(payload): Json<RevokeRequest>,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let cfg = signing_config_from_env();
    let Ok(key_ring) = key_ring_from_env() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"signing_keys_unavailable"})),
        );
    };

    let Ok(claims) = decode_with_key_ring(&payload.token, &key_ring, &cfg) else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid_token"})),
        );
    };

    let ttl = ttl_from_exp(claims.exp);
    if let Err(err) = state
        .runtime
        .store
        .set(&format!("auth:revoked:{}", claims.jti), "1", ttl)
        .await
    {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"store_unavailable","detail":err.to_string()})),
        );
    }

    if claims.token_use == "refresh" {
        if let Err(err) = state
            .runtime
            .store
            .set(&format!("auth:refresh:used:{}", claims.jti), "1", ttl)
            .await
        {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"store_unavailable","detail":err.to_string()})),
            );
        }
    }

    (
        axum::http::StatusCode::OK,
        Json(json!({"status":"revoked","token_use":claims.token_use})),
    )
}

async fn jwks_handler() -> (axum::http::StatusCode, Json<serde_json::Value>) {
    match key_ring_from_env() {
        Ok(keys) => {
            let payload = keys
                .keys
                .keys()
                .cloned()
                .map(|kid| JwkDescriptor {
                    kty: "oct",
                    kid,
                    alg: "HS256",
                    r#use: "sig",
                })
                .collect::<Vec<JwkDescriptor>>();
            (axum::http::StatusCode::OK, Json(json!({"keys": payload})))
        }
        Err(err) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"jwks_unavailable","detail":err.to_string()})),
        ),
    }
}

async fn auth_status_handler() -> (axum::http::StatusCode, Json<serde_json::Value>) {
    match key_ring_from_env() {
        Ok(keys) => (
            axum::http::StatusCode::OK,
            Json(json!({
                "status":"ok",
                "active_kid": keys.active_kid,
                "key_count": keys.keys.len(),
                "auth_mode": std::env::var("KRAB_AUTH_MODE").unwrap_or_else(|_| "jwt".to_string())
            })),
        ),
        Err(err) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status":"degraded","detail":err.to_string()})),
        ),
    }
}

async fn auth_capabilities_handler() -> Json<serde_json::Value> {
    Json(json!({
        "service": "auth",
        "default_protocol": "rest",
        "supported_protocols": ["rest"],
        "protocol_routes": {
            "rest": "/api/v1/auth"
        },
        "allow_client_override": false,
        "policy": {
            "restricted_operations": {
                "auth.login": ["rest"],
                "auth.refresh": ["rest"],
                "auth.revoke": ["rest"],
                "auth.jwks": ["rest"],
                "auth.status": ["rest"]
            }
        }
    }))
}

impl HasRuntimeState for AppState {
    fn runtime_state(&self) -> &RuntimeState {
        &self.runtime
    }
}

fn build_app(state: AppState) -> Router {
    let api = Router::new()
        .route("/private", get(private))
        .route("/auth/login", post(login_handler))
        .route("/auth/refresh", post(refresh_handler))
        .route("/auth/revoke", post(revoke_handler))
        .route("/auth/jwks", get(jwks_handler))
        .route("/auth/capabilities", get(auth_capabilities_handler))
        .route("/auth/status", get(auth_status_handler));

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/ready", get(readiness_with_dependencies::<AppState>))
        .route("/metrics", get(metrics::<AppState>))
        .route("/metrics/prometheus", get(metrics_prometheus::<AppState>))
        .nest("/api/v1", api);

    apply_common_http_layers(app, state.clone()).with_state(state)
}

#[async_trait]
impl ApiService for AuthService {
    async fn start(&self) -> Result<()> {
        let state = AppState {
            runtime: RuntimeState::try_new()?.with_protocol_config(
                self.config
                    .protocol
                    .clone()
                    .unwrap_or_else(ProtocolConfig::from_env),
            ),
            credentials: Arc::clone(&self.credentials),
        };

        let app = build_app(state);

        serve_with_graceful_shutdown(app, &self.config).await
    }
}

fn bootstrap_auth_service() -> Result<AuthService> {
    let protocol_cfg = ProtocolConfig::from_env();
    anyhow::ensure!(
        protocol_cfg.default_protocol == ProtocolKind::Rest
            && protocol_cfg
                .enabled_protocols
                .iter()
                .all(|p| *p == ProtocolKind::Rest),
        "auth service is REST-only: default='{}' enabled={:?}",
        protocol_cfg.default_protocol.as_str(),
        protocol_cfg
            .enabled_protocols
            .iter()
            .map(ProtocolKind::as_str)
            .collect::<Vec<&str>>()
    );

    let cfg = KrabConfig::from_env_checked("auth", 3001)
        .context("failed to load auth config from environment")?;
    let secrets_report = cfg
        .validate_all()
        .context("startup config validation failed")?;
    if !secrets_report.is_clean() {
        warn!(
            issue_count = secrets_report.issues.len(),
            "startup_secrets_policy_warnings_detected"
        );
    }
    let config = ServiceConfig {
        name: cfg.service_name.clone(),
        host: cfg.host.clone(),
        port: cfg.port,
        protocol: Some(protocol_cfg.clone()),
    };

    // Fail fast if critical security configuration is missing in production mode
    let auth_mode = std::env::var("KRAB_AUTH_MODE").unwrap_or_else(|_| "jwt".to_string());
    info!(%auth_mode, "auth_startup_mode_resolved");
    let env = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
    let local_like_env = local_like_environment();

    if auth_mode.eq_ignore_ascii_case("jwt") || auth_mode.eq_ignore_ascii_case("oidc") {
        let issuer_present = std::env::var_os("KRAB_OIDC_ISSUER").is_some();
        let audience_present = std::env::var_os("KRAB_OIDC_AUDIENCE").is_some();
        info!(
            issuer_present,
            audience_present, "auth_startup_jwt_env_validation"
        );

        std::env::var("KRAB_OIDC_ISSUER")
            .context("KRAB_OIDC_ISSUER must be set when KRAB_AUTH_MODE=jwt")?;
        std::env::var("KRAB_OIDC_AUDIENCE")
            .context("KRAB_OIDC_AUDIENCE must be set when KRAB_AUTH_MODE=jwt")?;

        if !local_like_env {
            let has_key_ring_json_file = env_non_empty("KRAB_JWT_KEYS_JSON_FILE").is_some();
            let has_jwt_secret_file = env_non_empty("KRAB_JWT_SECRET_FILE").is_some();
            let has_key_ring_json_vault = env_non_empty("KRAB_JWT_KEYS_JSON_VAULT_REF").is_some();
            let has_jwt_secret_vault = env_non_empty("KRAB_JWT_SECRET_VAULT_REF").is_some();
            let jwt_secret = read_env_or_file("KRAB_JWT_SECRET")?;
            anyhow::ensure!(
                has_key_ring_json_file
                    || has_jwt_secret_file
                    || has_key_ring_json_vault
                    || has_jwt_secret_vault,
                "non-local jwt/oidc mode requires vault or *_FILE sourcing: set KRAB_JWT_KEYS_JSON_FILE, KRAB_JWT_SECRET_FILE, KRAB_JWT_KEYS_JSON_VAULT_REF, or KRAB_JWT_SECRET_VAULT_REF"
            );

            if let Some(secret) = jwt_secret {
                anyhow::ensure!(
                    !secret.trim().is_empty() && secret != INSECURE_DEV_JWT_SECRET,
                    "non-local jwt/oidc mode forbids insecure/default KRAB_JWT_SECRET"
                );
            }

            let has_login_users_json_file =
                env_non_empty("KRAB_AUTH_LOGIN_USERS_JSON_FILE").is_some();
            let has_bootstrap_password_file =
                env_non_empty("KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE").is_some();
            let has_login_users_json_vault =
                env_non_empty("KRAB_AUTH_LOGIN_USERS_JSON_VAULT_REF").is_some();
            let has_bootstrap_password_vault =
                env_non_empty("KRAB_AUTH_BOOTSTRAP_PASSWORD_VAULT_REF").is_some();
            let bootstrap_password = read_env_or_file("KRAB_AUTH_BOOTSTRAP_PASSWORD")?;
            anyhow::ensure!(
                has_login_users_json_file
                    || has_bootstrap_password_file
                    || has_login_users_json_vault
                    || has_bootstrap_password_vault,
                "non-local auth mode requires vault or *_FILE sourcing: set KRAB_AUTH_LOGIN_USERS_JSON_FILE, KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE, KRAB_AUTH_LOGIN_USERS_JSON_VAULT_REF, or KRAB_AUTH_BOOTSTRAP_PASSWORD_VAULT_REF"
            );

            if let Some(password) = bootstrap_password {
                anyhow::ensure!(
                    !password.trim().is_empty() && password != INSECURE_DEV_BOOTSTRAP_PASSWORD,
                    "non-local auth mode forbids insecure/default KRAB_AUTH_BOOTSTRAP_PASSWORD"
                );
            }

            // Sourcing a secret from a file or vault says nothing about its
            // format. This is what stops a plaintext password reaching a
            // production login path through an otherwise-correct mount.
            enforce_hashed_credentials()?;
        }
    } else if auth_mode.eq_ignore_ascii_case("static") && !local_like_env {
        anyhow::bail!(
            "KRAB_AUTH_MODE=static is forbidden in '{}' environment; use jwt/oidc",
            env
        );
    }

    // Built here rather than per-request so a malformed credential map fails
    // startup instead of the first login attempt.
    let credentials = Arc::new(resolve_credential_store()?);
    info!(
        configured_users = credentials.len(),
        "auth_credential_store_resolved"
    );

    Ok(AuthService {
        config,
        credentials,
    })
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    init_tracing("service_auth");

    let service = bootstrap_auth_service()?;
    service.start().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::body::Body;
    use axum::http::header;
    use axum::http::Request;
    use axum::http::StatusCode;
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;
    use tower::util::ServiceExt;

    const TEST_BEARER_TOKEN: &str = "test-token";

    fn test_auth_header() -> String {
        let token =
            std::env::var("KRAB_BEARER_TOKEN").unwrap_or_else(|_| TEST_BEARER_TOKEN.to_string());
        format!("Bearer {token}")
    }

    const TEST_LOGIN_USER: &str = "admin";
    const TEST_LOGIN_PASSWORD: &str = "s3cret-for-tests";

    /// A store holding a single Argon2id-hashed credential.
    fn test_credential_store() -> Arc<EnvHashCredentialStore> {
        let hash = hash_password(TEST_LOGIN_PASSWORD).expect("hashing should succeed");
        let mut users = BTreeMap::new();
        users.insert(TEST_LOGIN_USER.to_string(), hash);
        Arc::new(EnvHashCredentialStore::from_map(users).expect("store should build"))
    }

    fn test_app() -> Router {
        std::env::set_var("KRAB_AUTH_MODE", "static");
        std::env::set_var("KRAB_BEARER_TOKEN", TEST_BEARER_TOKEN);
        build_app(AppState {
            runtime: RuntimeState::new(),
            credentials: test_credential_store(),
        })
    }

    async fn login_status(username: &str, password: &str) -> StatusCode {
        let app = test_app();
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "username": username, "password": password }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    /// 6.7 — a correct password authenticates.
    #[tokio::test]
    async fn credential_correct_password_is_accepted() {
        assert_ne!(
            login_status(TEST_LOGIN_USER, TEST_LOGIN_PASSWORD).await,
            StatusCode::UNAUTHORIZED,
            "a valid credential must not be rejected"
        );
    }

    /// 6.7 — a wrong password does not.
    #[tokio::test]
    async fn credential_wrong_password_is_rejected() {
        assert_eq!(
            login_status(TEST_LOGIN_USER, "not-the-password").await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// 6.7 — an unknown user is rejected the same way a wrong password is, and
    /// with the same status, so the response cannot be used to enumerate users.
    #[tokio::test]
    async fn credential_unknown_user_is_rejected_indistinguishably() {
        assert_eq!(
            login_status("no-such-user", TEST_LOGIN_PASSWORD).await,
            login_status(TEST_LOGIN_USER, "not-the-password").await
        );
    }

    /// 6.7 — plaintext in the credential map fails the non-local startup guard,
    /// even though it is well-formed JSON sourced correctly.
    #[test]
    fn credential_plaintext_map_fails_the_non_local_guard() {
        let _guard = credential_env_guard();
        std::env::set_var("KRAB_AUTH_LOGIN_USERS_JSON", r#"{"admin":"hunter2"}"#);

        let err = enforce_hashed_credentials()
            .expect_err("plaintext must not pass the guard")
            .to_string();

        assert!(err.contains("admin"), "error should name the user: {err}");
        assert!(
            err.contains("PHC"),
            "error should name the required format: {err}"
        );
        assert!(
            !err.contains("hunter2"),
            "error must not echo the secret: {err}"
        );
    }

    /// 6.7 — a properly hashed map passes the same guard.
    #[test]
    fn credential_hashed_map_passes_the_non_local_guard() {
        let _guard = credential_env_guard();
        let hash = hash_password(TEST_LOGIN_PASSWORD).unwrap();
        std::env::set_var(
            "KRAB_AUTH_LOGIN_USERS_JSON",
            json!({ "admin": hash }).to_string(),
        );

        assert!(enforce_hashed_credentials().is_ok());
    }

    /// 6.7 — a plaintext `KRAB_AUTH_BOOTSTRAP_PASSWORD` is caught too. The
    /// pre-existing `change-me` rejection covers only that one literal; this
    /// covers every other plaintext value.
    #[test]
    fn credential_plaintext_bootstrap_password_fails_the_non_local_guard() {
        let _guard = credential_env_guard();
        std::env::set_var("KRAB_AUTH_BOOTSTRAP_PASSWORD", "a-strong-looking-plaintext");

        let err = enforce_hashed_credentials()
            .expect_err("plaintext must not pass the guard")
            .to_string();
        assert!(err.contains("PHC"), "unhelpful error: {err}");
    }

    /// 6.7 — the `local` convenience path still works: an unhashed dev password
    /// is hashed at startup rather than compared in plaintext.
    #[test]
    fn credential_local_environment_hashes_plaintext_at_startup() {
        let _guard = credential_env_guard();
        std::env::set_var("KRAB_ENVIRONMENT", "local");
        std::env::set_var(
            "KRAB_AUTH_BOOTSTRAP_PASSWORD",
            INSECURE_DEV_BOOTSTRAP_PASSWORD,
        );

        let store = resolve_credential_store().expect("local startup should succeed");
        assert_eq!(store.len(), 1);
    }

    /// 6.7 — outside `local`, that same convenience is refused.
    #[test]
    fn credential_non_local_environment_refuses_to_hash_plaintext() {
        let _guard = credential_env_guard();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_BOOTSTRAP_PASSWORD", "some-plaintext");

        assert!(
            resolve_credential_store().is_err(),
            "non-local startup must not accept a plaintext credential"
        );
    }

    const CREDENTIAL_ENV_VARS: &[&str] = &[
        "KRAB_ENVIRONMENT",
        "KRAB_AUTH_LOGIN_USERS_JSON",
        "KRAB_AUTH_BOOTSTRAP_PASSWORD",
        "KRAB_AUTH_BOOTSTRAP_USER",
    ];

    /// Saves and restores the credential environment, and clears it for the
    /// duration of a test. These tests mutate process-global state, so they are
    /// serialised against each other.
    fn credential_env_guard() -> CredentialEnvGuard {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let saved = CREDENTIAL_ENV_VARS
            .iter()
            .map(|name| (*name, std::env::var(name).ok()))
            .collect();
        for name in CREDENTIAL_ENV_VARS {
            std::env::remove_var(name);
        }

        CredentialEnvGuard {
            saved,
            _lock: guard,
        }
    }

    struct CredentialEnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for CredentialEnvGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[tokio::test]
    async fn integration_health_endpoint() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_capabilities_endpoint_returns_rest_only() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload.get("service").and_then(Value::as_str), Some("auth"));
        assert_eq!(
            payload
                .get("supported_protocols")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
                .and_then(Value::as_str),
            Some("rest")
        );
        assert_eq!(
            payload.get("default_protocol").and_then(Value::as_str),
            Some("rest")
        );
    }

    #[tokio::test]
    async fn contract_protected_route_requires_auth() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/private")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn e2e_auth_lifecycle_static_token_allows_protected_route() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/private")
                    .header(header::AUTHORIZATION, test_auth_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(String::from_utf8(bytes.to_vec()).unwrap(), "private_ok");
    }

    #[tokio::test]
    async fn test_existing_lifecycle_unaffected() {
        let app = test_app();

        let login_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "username": TEST_LOGIN_USER,
                            "password": TEST_LOGIN_PASSWORD,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);
        let login_bytes = to_bytes(login_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let login_payload: Value = serde_json::from_slice(&login_bytes).unwrap();
        let refresh_token = login_payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();

        let refresh_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/refresh")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(format!(
                        r#"{{"refresh_token":"{}"}}"#,
                        refresh_token
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refresh_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn security_lockdown_auth_ops_not_exposed_via_graphql_or_rpc_paths() {
        std::env::set_var("KRAB_PROTOCOL_TOPOLOGY", "split_services");
        std::env::set_var("KRAB_PROTOCOL_EXPOSURE_MODE", "multi");
        std::env::set_var("KRAB_PROTOCOL_ENABLED", "rest,graphql,rpc");

        let app = test_app();

        let graphql_attempt = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/graphql")
                    .header(header::AUTHORIZATION, test_auth_header())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"query":"{ authStatus { status } }"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // As of 0.3.0, a request to a disabled route-family protocol is
        // rejected up front with 400 PROTOCOL_NOT_SUPPORTED instead of falling
        // through to a 404. service_auth exposes REST only, so the graphql
        // family is disabled — the lockdown holds, now as an explicit rejection.
        assert_eq!(graphql_attempt.status(), StatusCode::BAD_REQUEST);

        let rpc_attempt = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/rpc")
                    .header(header::AUTHORIZATION, test_auth_header())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"method":"auth.login","params":{},"id":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Same as the graphql path: the rpc family is disabled on this
        // REST-only service and is now rejected with 400, not 404.
        assert_eq!(rpc_attempt.status(), StatusCode::BAD_REQUEST);

        std::env::remove_var("KRAB_PROTOCOL_TOPOLOGY");
        std::env::remove_var("KRAB_PROTOCOL_EXPOSURE_MODE");
        std::env::remove_var("KRAB_PROTOCOL_ENABLED");
    }

    #[test]
    fn startup_fails_when_auth_protocol_config_is_not_rest_only() {
        std::env::set_var("KRAB_PROTOCOL_EXPOSURE_MODE", "multi");
        std::env::set_var("KRAB_PROTOCOL_ENABLED", "rest,graphql");
        std::env::set_var("KRAB_PROTOCOL_DEFAULT", "graphql");

        let result = bootstrap_auth_service();
        assert!(
            result.is_err(),
            "bootstrap should fail for non-rest auth protocol config"
        );

        std::env::remove_var("KRAB_PROTOCOL_EXPOSURE_MODE");
        std::env::remove_var("KRAB_PROTOCOL_ENABLED");
        std::env::remove_var("KRAB_PROTOCOL_DEFAULT");
    }

    #[tokio::test]
    async fn fault_injection_auth_failure_burst_is_rate_limited() {
        let app = test_app();
        let mut saw_too_many_requests = false;

        for _ in 0..130 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/private")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                saw_too_many_requests = true;
                break;
            }
        }

        assert!(
            saw_too_many_requests,
            "expected auth failure limiter to emit HTTP 429 under burst failures"
        );
    }

    /// The endpoint exists and serves Prometheus text — but not to anonymous
    /// callers. This asserted a plain 200 while `/metrics/prometheus` was on
    /// the default open-path list; that default was the leak, so the test that
    /// locked it in had to go with it. Both halves are asserted here so the
    /// contract cannot regress in either direction: closed without the flag,
    /// and still serving the expected payload with it.
    /// No `#[serial]`: this file has no serialised tests, so the attribute
    /// would only order this one against an empty set. `KRAB_METRICS_PUBLIC`
    /// is touched by no other test here, and it is cleared on both exits.
    #[tokio::test]
    async fn contract_metrics_prometheus_requires_auth_and_opens_with_flag() {
        std::env::remove_var("KRAB_METRICS_PUBLIC");
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/metrics/prometheus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "metrics must not be anonymous by default"
        );

        std::env::set_var("KRAB_METRICS_PUBLIC", "true");
        let response = test_app()
            .oneshot(
                Request::builder()
                    .uri("/metrics/prometheus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("krab_requests_total"));
        std::env::remove_var("KRAB_METRICS_PUBLIC");
    }

    #[tokio::test]
    async fn contract_ready_returns_dependencies_shape() {
        let app = test_app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let deps = payload
            .get("dependencies")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(deps
            .iter()
            .any(|d| d.get("name") == Some(&Value::String("auth_runtime".to_string()))));
    }

    #[tokio::test]
    async fn e2e_network_private_route_static_auth_over_tcp() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = test_app();
        let (tx, rx) = oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET /api/v1/private HTTP/1.1\r\nHost: localhost\r\nAuthorization: {}\r\nConnection: close\r\n\r\n",
            test_auth_header()
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response = String::from_utf8(buf).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("private_ok"));

        let _ = tx.send(());
        let _ = server.await;
    }
}
