use anyhow::{Context, Result};
use std::fs;

// ---------------------------------------------------------------------------
// Secrets source policy
// ---------------------------------------------------------------------------

/// Severity of a secret-source policy violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretIssueSeverity {
    /// Hard failure — blocks startup in the affected environment.
    Error,
    /// Advisory — logged but does not block startup.
    Warning,
}

/// A single secret-source policy violation.
#[derive(Debug, Clone)]
pub struct SecretIssue {
    /// The environment variable name that violated policy.
    pub var_name: String,
    /// Machine-readable policy rule code (e.g. `INLINE_SECRET_IN_PROD`).
    pub policy_rule: String,
    pub severity: SecretIssueSeverity,
    /// Human-readable description of the violation.
    pub reason: String,
}

impl std::fmt::Display for SecretIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            SecretIssueSeverity::Error => "ERROR",
            SecretIssueSeverity::Warning => "WARN",
        };
        write!(
            f,
            "[{}] {} ({}): {}",
            sev, self.var_name, self.policy_rule, self.reason
        )
    }
}

/// Aggregated result of secrets-source policy validation.
#[derive(Debug, Clone)]
pub struct SecretsValidationReport {
    pub issues: Vec<SecretIssue>,
}

impl SecretsValidationReport {
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.severity == SecretIssueSeverity::Error)
    }

    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Per-environment secrets source rules.
///
/// - **Dev**: any source allowed (inline env, `*_FILE`, `*_VAULT_REF`).
/// - **Staging**: `*_FILE` / `*_VAULT_REF` preferred; inline triggers a warning.
/// - **Prod** (or unknown): only `*_FILE` / `*_VAULT_REF`; inline is an error.
fn check_secret_source(var_name: &str, env: &Environment, issues: &mut Vec<SecretIssue>) {
    let has_inline = env_non_empty(var_name).is_some();
    let has_file = env_non_empty(&format!("{var_name}_FILE")).is_some();
    let has_vault = env_non_empty(&format!("{var_name}_VAULT_REF")).is_some();

    // If the secret is not set at all, nothing to validate here (presence
    // checks are done separately by the caller when the secret is required).
    if !has_inline && !has_file && !has_vault {
        return;
    }

    let has_secure = has_file || has_vault;

    match env {
        Environment::Dev => { /* anything goes */ }
        Environment::Staging => {
            if has_vault {
                issues.push(SecretIssue {
                    var_name: var_name.to_string(),
                    policy_rule: "UNRESOLVED_VAULT_REF_IN_STAGING".to_string(),
                    severity: SecretIssueSeverity::Error,
                    reason: format!(
                        "{var_name}_VAULT_REF is set in staging, but runtime vault resolution is not configured; materialize the secret before startup"
                    ),
                });
            }
            if has_inline && !has_secure {
                issues.push(SecretIssue {
                    var_name: var_name.to_string(),
                    policy_rule: "INLINE_SECRET_IN_STAGING".to_string(),
                    severity: SecretIssueSeverity::Warning,
                    reason: format!(
                        "{var_name} is set as an inline env var in staging; \
                         prefer *_FILE or *_VAULT_REF for secret sourcing"
                    ),
                });
            }
        }
        Environment::Prod | Environment::Unknown(_) => {
            if has_vault {
                issues.push(SecretIssue {
                    var_name: var_name.to_string(),
                    policy_rule: "UNRESOLVED_VAULT_REF_IN_PROD".to_string(),
                    severity: SecretIssueSeverity::Error,
                    reason: format!(
                        "{var_name}_VAULT_REF is set in '{}', but runtime vault resolution is not configured; materialize the secret before startup",
                        env.as_str()
                    ),
                });
            }
            if has_inline && !has_secure {
                issues.push(SecretIssue {
                    var_name: var_name.to_string(),
                    policy_rule: "INLINE_SECRET_IN_PROD".to_string(),
                    severity: SecretIssueSeverity::Error,
                    reason: format!(
                        "{var_name} is set as an inline env var in '{}'; \
                         only *_FILE or *_VAULT_REF sourcing is allowed",
                        env.as_str()
                    ),
                });
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Environment {
    Dev,
    Staging,
    Prod,
    Unknown(String),
}

pub fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn read_env_or_file(name: &str) -> Result<Option<String>> {
    if let Some(value) = env_non_empty(name) {
        return Ok(Some(value));
    }

    let file_var = format!("{name}_FILE");
    if let Some(path) = env_non_empty(&file_var) {
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {} from file path '{}'", file_var, path))?;
        let value = raw.trim().to_string();
        anyhow::ensure!(
            !value.is_empty(),
            "{} points to empty file '{}'",
            file_var,
            path
        );
        return Ok(Some(value));
    }

    let vault_ref_var = format!("{name}_VAULT_REF");
    if let Some(vault_ref) = env_non_empty(&vault_ref_var) {
        anyhow::bail!(
            "{} is set to '{}' but runtime vault resolution is not configured; materialize the secret before startup or provide {} / {}_FILE",
            vault_ref_var,
            vault_ref,
            name,
            name
        );
    }

    Ok(None)
}

impl Environment {
    pub fn from_env() -> Self {
        match std::env::var("KRAB_ENVIRONMENT") {
            Ok(v) if v.eq_ignore_ascii_case("dev") => Self::Dev,
            Ok(v) if v.eq_ignore_ascii_case("staging") => Self::Staging,
            Ok(v) if v.eq_ignore_ascii_case("prod") || v.eq_ignore_ascii_case("production") => {
                Self::Prod
            }
            Ok(v) => Self::Unknown(v),
            Err(_) => Self::Dev,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
            Self::Unknown(v) => v.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub auth_mode: String,
    pub service_auth_scope: String,
    pub rate_limit_capacity: u64,
    pub rate_limit_refill_per_sec: u64,
    /// Behavior when distributed rate-limit store errors occur.
    /// true = fail-open, false = fail-closed.
    pub rate_limit_fail_open: bool,
    /// Trust `x-forwarded-for` / `x-real-ip` request headers for client IP extraction.
    pub trust_proxy_headers: bool,
    /// Allowed CORS origins from `KRAB_CORS_ORIGINS` (comma-separated).
    /// In staging/prod, this must be non-empty.
    pub cors_origins: Vec<String>,
    /// Whether runtime may treat CORS as allow-all when no explicit origin list exists.
    /// This is only enabled in `dev` by default.
    pub cors_allow_any_origin: bool,
}

impl HttpConfig {
    pub fn from_env() -> Self {
        let environment = Environment::from_env();
        let env_bool = |name: &str| {
            std::env::var(name)
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .ok()
        };
        let cors_origins = std::env::var("KRAB_CORS_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        Self {
            auth_mode: std::env::var("KRAB_AUTH_MODE").unwrap_or_else(|_| "jwt".to_string()),
            service_auth_scope: std::env::var("KRAB_SERVICE_AUTH_SCOPE")
                .unwrap_or_else(|_| "service:internal".to_string()),
            rate_limit_capacity: std::env::var("KRAB_RATE_LIMIT_CAPACITY")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(120),
            rate_limit_refill_per_sec: std::env::var("KRAB_RATE_LIMIT_REFILL_PER_SEC")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60),
            rate_limit_fail_open: env_bool("KRAB_RATE_LIMIT_FAIL_OPEN")
                .unwrap_or(matches!(environment, Environment::Dev)),
            trust_proxy_headers: env_bool("KRAB_TRUST_PROXY_HEADERS").unwrap_or(false),
            cors_origins,
            cors_allow_any_origin: matches!(environment, Environment::Dev),
        }
    }
}

/// Unified application configuration loaded from environment variables.
///
/// Call [`KrabConfig::from_env`] once at startup; pass the result (or
/// specific sub-configs) through the dependency graph instead of reading
/// `std::env::var` ad-hoc in individual modules.
#[derive(Debug, Clone)]
pub struct KrabConfig {
    pub environment: Environment,
    pub service_name: String,
    pub host: String,
    pub port: u16,
    pub log_filter: String,
    pub http: HttpConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    InvalidPort { raw: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPort { raw } => {
                write!(f, "invalid KRAB_PORT='{}': expected integer 0-65535", raw)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

fn parse_port_from_env(default_port: u16) -> Result<u16, ConfigError> {
    match std::env::var("KRAB_PORT") {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(default_port)
            } else {
                trimmed
                    .parse::<u16>()
                    .map_err(|_| ConfigError::InvalidPort {
                        raw: trimmed.to_string(),
                    })
            }
        }
        Err(_) => Ok(default_port),
    }
}

impl KrabConfig {
    /// Load all configuration from environment variables with typed defaults.
    pub fn from_env(default_service_name: &str, default_port: u16) -> Self {
        Self::from_env_checked(default_service_name, default_port)
            .expect("KrabConfig::from_env should only be used where invalid config is unrecoverable; prefer from_env_checked")
    }

    pub fn from_env_checked(
        default_service_name: &str,
        default_port: u16,
    ) -> Result<Self, ConfigError> {
        let port = parse_port_from_env(default_port)?;

        Ok(Self {
            environment: Environment::from_env(),
            service_name: std::env::var("KRAB_SERVICE_NAME")
                .unwrap_or_else(|_| default_service_name.to_string()),
            host: std::env::var("KRAB_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port,
            log_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            http: HttpConfig::from_env(),
        })
    }

    /// Validate security-critical configuration at startup.
    ///
    /// In `Dev` environments all checks are skipped. In `Staging`, `Prod`, or any
    /// unrecognised environment, required secrets must be present or this returns
    /// an error — callers should propagate the error and abort the process.
    pub fn validate(&self) -> anyhow::Result<()> {
        let has_non_empty = |name: &str| !std::env::var(name).unwrap_or_default().trim().is_empty();

        match self.environment {
            Environment::Dev => return Ok(()),
            Environment::Staging | Environment::Prod | Environment::Unknown(_) => {}
        }

        if matches!(self.environment, Environment::Staging | Environment::Prod)
            && self.http.cors_origins.is_empty()
        {
            anyhow::bail!(
                "KRAB_CORS_ORIGINS must be explicitly configured in '{}' environment; refusing wildcard CORS",
                self.environment.as_str()
            );
        }

        let auth_mode = self.http.auth_mode.as_str();
        if auth_mode.eq_ignore_ascii_case("static") {
            anyhow::bail!(
                "KRAB_AUTH_MODE=static is not allowed in '{}' environment; use KRAB_AUTH_MODE=jwt (or oidc) with provider-based validation",
                self.environment.as_str()
            );
        } else if auth_mode.eq_ignore_ascii_case("jwt") || auth_mode.eq_ignore_ascii_case("oidc") {
            let has_provider_json = has_non_empty("KRAB_JWT_PROVIDERS_JSON");
            let has_provider_json_file = has_non_empty("KRAB_JWT_PROVIDERS_JSON_FILE");
            let has_provider_json_vault_ref = has_non_empty("KRAB_JWT_PROVIDERS_JSON_VAULT_REF");
            let token = std::env::var("KRAB_BEARER_TOKEN").unwrap_or_default();
            if !token.trim().is_empty() {
                anyhow::bail!(
                    "KRAB_BEARER_TOKEN must be unset/empty in '{}' environment when KRAB_AUTH_MODE={} \
                     (static bearer tokens are not allowed outside dev)",
                    self.environment.as_str(),
                    auth_mode
                );
            }

            let has_keys = has_non_empty("KRAB_JWT_KEYS_JSON");
            let has_secret = has_non_empty("KRAB_JWT_SECRET");
            let has_keys_file = has_non_empty("KRAB_JWT_KEYS_JSON_FILE");
            let has_secret_file = has_non_empty("KRAB_JWT_SECRET_FILE");
            let has_keys_vault_ref = has_non_empty("KRAB_JWT_KEYS_JSON_VAULT_REF");
            let has_secret_vault_ref = has_non_empty("KRAB_JWT_SECRET_VAULT_REF");
            let has_issuer = has_non_empty("KRAB_OIDC_ISSUER");
            let has_audience = has_non_empty("KRAB_OIDC_AUDIENCE");

            let has_secure_secret_source =
                has_keys_file || has_secret_file || has_keys_vault_ref || has_secret_vault_ref;

            if (has_keys || has_secret) && !has_secure_secret_source {
                anyhow::bail!(
                    "In '{}' environment, inline KRAB_JWT_SECRET/KRAB_JWT_KEYS_JSON is forbidden; use *_FILE or *_VAULT_REF secret sourcing",
                    self.environment.as_str()
                );
            }

            let has_provider_bundle =
                has_provider_json || has_provider_json_file || has_provider_json_vault_ref;
            let has_fallback_provider_tuple =
                (has_keys || has_secret || has_keys_file || has_secret_file)
                    && has_issuer
                    && has_audience;

            if !has_provider_bundle && !has_fallback_provider_tuple {
                anyhow::bail!(
                    "JWT/OIDC provider configuration required in '{}' environment; set KRAB_JWT_PROVIDERS_JSON \
                     or provide KRAB_OIDC_ISSUER + KRAB_OIDC_AUDIENCE + secure secret sourcing via \
                     KRAB_JWT_SECRET_FILE/KRAB_JWT_KEYS_JSON_FILE (or *_VAULT_REF)",
                    self.environment.as_str()
                );
            }
        } else {
            anyhow::bail!(
                "Unsupported KRAB_AUTH_MODE='{}' in '{}' environment; use jwt or oidc",
                auth_mode,
                self.environment.as_str()
            );
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Secrets-source policy validation
    // -----------------------------------------------------------------------

    /// Well-known secret env vars checked by the secrets-source policy.
    const SECRET_VARS: &'static [&'static str] = &[
        "DATABASE_URL",
        "KRAB_REDIS_URL",
        "KRAB_SMTP_PASSWORD",
        "KRAB_JWT_SECRET",
        "KRAB_JWT_KEYS_JSON",
        "KRAB_JWT_PROVIDERS_JSON",
        "KRAB_BEARER_TOKEN",
    ];

    /// Validate that every secret variable is sourced according to the
    /// environment's secrets policy. Returns a structured report with
    /// per-variable issues and explicit failure reasons.
    pub fn validate_secrets_sources(&self) -> SecretsValidationReport {
        let mut issues = Vec::new();

        for var in Self::SECRET_VARS {
            check_secret_source(var, &self.environment, &mut issues);
        }

        // Extra rule: KRAB_BEARER_TOKEN must not be populated in non-dev
        if !matches!(self.environment, Environment::Dev)
            && env_non_empty("KRAB_BEARER_TOKEN").is_some()
        {
            issues.push(SecretIssue {
                var_name: "KRAB_BEARER_TOKEN".to_string(),
                policy_rule: "STATIC_TOKEN_IN_NON_DEV".to_string(),
                severity: SecretIssueSeverity::Error,
                reason: format!(
                    "KRAB_BEARER_TOKEN must be unset in '{}' environment; \
                     static bearer tokens are only allowed in dev",
                    self.environment.as_str()
                ),
            });
        }

        SecretsValidationReport { issues }
    }

    /// Combined validation: runs both the existing auth/CORS checks **and**
    /// the secrets-source policy. Returns the first hard error encountered.
    pub fn validate_all(&self) -> anyhow::Result<SecretsValidationReport> {
        // Run auth & CORS checks (existing logic).
        self.validate()?;

        // Run secrets-source policy checks.
        let report = self.validate_secrets_sources();
        if report.has_errors() {
            let first_error = report
                .issues
                .iter()
                .find(|i| i.severity == SecretIssueSeverity::Error)
                .unwrap();
            anyhow::bail!(
                "Secrets policy violation: {} ({})",
                first_error.reason,
                first_error.policy_rule
            );
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.get_or_init(|| Mutex::new(())).lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn clear_auth_env() {
        for key in [
            "KRAB_ENVIRONMENT",
            "KRAB_AUTH_MODE",
            "KRAB_BEARER_TOKEN",
            "KRAB_JWT_SECRET",
            "KRAB_JWT_SECRET_FILE",
            "KRAB_JWT_SECRET_VAULT_REF",
            "KRAB_JWT_KEYS_JSON",
            "KRAB_JWT_KEYS_JSON_FILE",
            "KRAB_JWT_KEYS_JSON_VAULT_REF",
            "KRAB_JWT_PROVIDERS_JSON",
            "KRAB_JWT_PROVIDERS_JSON_FILE",
            "KRAB_JWT_PROVIDERS_JSON_VAULT_REF",
            "KRAB_OIDC_ISSUER",
            "KRAB_OIDC_AUDIENCE",
            "KRAB_CORS_ORIGINS",
            "KRAB_TRUST_PROXY_HEADERS",
            "KRAB_RATE_LIMIT_FAIL_OPEN",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    #[serial]
    fn validate_rejects_static_in_non_local_env() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        std::env::set_var("KRAB_AUTH_MODE", "static");
        std::env::set_var("KRAB_BEARER_TOKEN", "token");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("KRAB_AUTH_MODE=static is not allowed"));
    }

    #[test]
    #[serial]
    fn validate_requires_provider_configuration_in_non_local_jwt_mode() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("JWT/OIDC provider configuration required"));
    }

    #[test]
    #[serial]
    fn validate_accepts_static_mode_in_dev() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "dev");
        std::env::set_var("KRAB_AUTH_MODE", "static");
        std::env::set_var("KRAB_BEARER_TOKEN", "token");

        let cfg = KrabConfig::from_env("users", 3002);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    #[serial]
    fn validate_accepts_oidc_tuple_in_non_local_env() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        std::env::set_var("KRAB_AUTH_MODE", "oidc");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_JWT_SECRET_FILE", "/run/secrets/krab_jwt_secret");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");

        let cfg = KrabConfig::from_env("users", 3002);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    #[serial]
    fn validate_rejects_inline_secret_in_non_local_env() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_JWT_SECRET", "secret-inline");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("inline KRAB_JWT_SECRET/KRAB_JWT_KEYS_JSON is forbidden"));
    }

    #[test]
    #[serial]
    fn validate_rejects_empty_cors_origins_in_staging() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_JWT_SECRET_FILE", "/run/secrets/krab_jwt_secret");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("KRAB_CORS_ORIGINS must be explicitly configured"));
    }

    #[test]
    #[serial]
    fn validate_accepts_non_empty_cors_origins_in_staging() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_JWT_SECRET_FILE", "/run/secrets/krab_jwt_secret");
        std::env::set_var(
            "KRAB_CORS_ORIGINS",
            "https://app.example.com,https://admin.example.com",
        );

        let cfg = KrabConfig::from_env("users", 3002);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    #[serial]
    fn from_env_rejects_invalid_port_value() {
        let _guard = env_lock();
        clear_auth_env();
        std::env::set_var("KRAB_PORT", "invalid-port");

        let err = KrabConfig::from_env_checked("users", 3002)
            .expect_err("invalid KRAB_PORT should return a typed config error");

        assert_eq!(
            err,
            ConfigError::InvalidPort {
                raw: "invalid-port".to_string()
            }
        );
        assert!(err.to_string().contains("invalid KRAB_PORT='invalid-port'"));
        std::env::remove_var("KRAB_PORT");
    }

    // -----------------------------------------------------------------------
    // Secrets source policy tests
    // -----------------------------------------------------------------------

    fn clear_secrets_env() {
        clear_auth_env();
        for key in [
            "DATABASE_URL",
            "DATABASE_URL_FILE",
            "DATABASE_URL_VAULT_REF",
            "KRAB_REDIS_URL",
            "KRAB_REDIS_URL_FILE",
            "KRAB_REDIS_URL_VAULT_REF",
            "KRAB_SMTP_PASSWORD",
            "KRAB_SMTP_PASSWORD_FILE",
            "KRAB_SMTP_PASSWORD_VAULT_REF",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    #[serial]
    fn secrets_policy_allows_inline_in_dev() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "dev");
        std::env::set_var("DATABASE_URL", "postgres://localhost/krab");
        std::env::set_var("KRAB_REDIS_URL", "redis://localhost");

        let cfg = KrabConfig::from_env("users", 3002);
        let report = cfg.validate_secrets_sources();
        assert!(report.is_clean(), "dev should allow inline secrets");
    }

    #[test]
    #[serial]
    fn secrets_policy_warns_inline_in_staging() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "staging");
        std::env::set_var("DATABASE_URL", "postgres://staging/krab");

        let cfg = KrabConfig::from_env("users", 3002);
        let report = cfg.validate_secrets_sources();
        assert!(!report.is_clean());
        assert!(
            !report.has_errors(),
            "staging inline should warn, not error"
        );
        let issue = &report.issues[0];
        assert_eq!(issue.var_name, "DATABASE_URL");
        assert_eq!(issue.policy_rule, "INLINE_SECRET_IN_STAGING");
        assert_eq!(issue.severity, SecretIssueSeverity::Warning);
    }

    #[test]
    #[serial]
    fn secrets_policy_rejects_inline_in_prod() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("DATABASE_URL", "postgres://prod/krab");

        let cfg = KrabConfig::from_env("users", 3002);
        let report = cfg.validate_secrets_sources();
        assert!(report.has_errors());
        let issue = report
            .issues
            .iter()
            .find(|i| i.var_name == "DATABASE_URL")
            .expect("expected DATABASE_URL issue");
        assert_eq!(issue.policy_rule, "INLINE_SECRET_IN_PROD");
        assert_eq!(issue.severity, SecretIssueSeverity::Error);
    }

    #[test]
    #[serial]
    fn secrets_policy_accepts_file_sourced_in_prod() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("DATABASE_URL_FILE", "/run/secrets/db_url");

        let cfg = KrabConfig::from_env("users", 3002);
        let report = cfg.validate_secrets_sources();
        let db_issues: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.var_name == "DATABASE_URL")
            .collect();
        assert!(
            db_issues.is_empty(),
            "file-sourced secret should pass in prod"
        );
    }

    #[test]
    #[serial]
    fn secrets_policy_report_contains_explicit_reasons() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("DATABASE_URL", "postgres://prod/krab");
        std::env::set_var("KRAB_REDIS_URL", "redis://prod");

        let cfg = KrabConfig::from_env("users", 3002);
        let report = cfg.validate_secrets_sources();
        assert!(report.issues.len() >= 2);
        for issue in &report.issues {
            assert!(
                !issue.reason.is_empty(),
                "every issue must have a human-readable reason"
            );
            assert!(
                !issue.policy_rule.is_empty(),
                "every issue must have a machine-readable policy rule"
            );
        }
    }

    #[test]
    #[serial]
    fn read_env_or_file_prefers_inline_value_over_file() {
        let _guard = env_lock();
        std::env::set_var("KRAB_TEST_SECRET", "inline-secret");
        std::env::set_var("KRAB_TEST_SECRET_FILE", "ignored-file-path");

        let resolved = read_env_or_file("KRAB_TEST_SECRET").expect("inline secret should resolve");
        assert_eq!(resolved.as_deref(), Some("inline-secret"));

        std::env::remove_var("KRAB_TEST_SECRET");
        std::env::remove_var("KRAB_TEST_SECRET_FILE");
    }

    #[test]
    #[serial]
    fn read_env_or_file_reads_file_when_inline_missing() {
        let _guard = env_lock();
        std::env::remove_var("KRAB_TEST_SECRET");

        let path = std::env::current_dir().unwrap().join(format!(
            "krab_test_secret_{}_{}.txt",
            std::process::id(),
            1
        ));
        std::fs::write(&path, "file-secret\n").expect("failed to write temp secret file");
        std::env::set_var("KRAB_TEST_SECRET_FILE", path.to_string_lossy().to_string());

        let resolved = read_env_or_file("KRAB_TEST_SECRET").expect("file secret should resolve");
        assert_eq!(resolved.as_deref(), Some("file-secret"));

        std::env::remove_var("KRAB_TEST_SECRET_FILE");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[serial]
    fn read_env_or_file_rejects_empty_file_contents() {
        let _guard = env_lock();
        std::env::remove_var("KRAB_TEST_SECRET");

        let path = std::env::current_dir().unwrap().join(format!(
            "krab_test_secret_empty_{}_{}.txt",
            std::process::id(),
            1
        ));
        std::fs::write(&path, "\n\n").expect("failed to write empty temp secret file");
        std::env::set_var("KRAB_TEST_SECRET_FILE", path.to_string_lossy().to_string());

        let err = read_env_or_file("KRAB_TEST_SECRET")
            .unwrap_err()
            .to_string();
        assert!(err.contains("KRAB_TEST_SECRET_FILE points to empty file"));

        std::env::remove_var("KRAB_TEST_SECRET_FILE");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    #[serial]
    fn read_env_or_file_rejects_unresolved_vault_ref() {
        let _guard = env_lock();
        std::env::remove_var("KRAB_TEST_SECRET");
        std::env::remove_var("KRAB_TEST_SECRET_FILE");
        std::env::set_var("KRAB_TEST_SECRET_VAULT_REF", "vault://kv/krab/test-secret");

        let err = read_env_or_file("KRAB_TEST_SECRET")
            .unwrap_err()
            .to_string();
        assert!(err.contains("KRAB_TEST_SECRET_VAULT_REF is set"));
        assert!(err.contains("runtime vault resolution is not configured"));

        std::env::remove_var("KRAB_TEST_SECRET_VAULT_REF");
    }

    #[test]
    #[serial]
    fn validate_all_rejects_prod_inline_secret_even_with_oidc_tuple_present() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");
        std::env::set_var("KRAB_JWT_SECRET", "inline-secret");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate_all().unwrap_err().to_string();
        assert!(err.contains("inline KRAB_JWT_SECRET/KRAB_JWT_KEYS_JSON is forbidden"));
    }

    #[test]
    #[serial]
    fn validate_all_rejects_prod_static_bearer_even_when_file_secret_present() {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_OIDC_ISSUER", "https://issuer.example.com");
        std::env::set_var("KRAB_OIDC_AUDIENCE", "krab-api");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");
        std::env::set_var("KRAB_JWT_SECRET_FILE", "/run/secrets/krab_jwt_secret");
        std::env::set_var("KRAB_BEARER_TOKEN", "still-not-allowed");

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate_all().unwrap_err().to_string();
        assert!(err.contains("KRAB_BEARER_TOKEN must be unset/empty"));
    }

    #[test]
    #[serial]
    fn validate_all_rejects_prod_unresolved_provider_vault_ref_without_materialized_secret_source()
    {
        let _guard = env_lock();
        clear_secrets_env();
        std::env::set_var("KRAB_ENVIRONMENT", "prod");
        std::env::set_var("KRAB_AUTH_MODE", "jwt");
        std::env::set_var("KRAB_CORS_ORIGINS", "https://app.example.com");
        std::env::set_var(
            "KRAB_JWT_PROVIDERS_JSON_VAULT_REF",
            "vault://kv/krab/providers-json",
        );

        let cfg = KrabConfig::from_env("users", 3002);
        let err = cfg.validate_all().unwrap_err().to_string();
        assert!(
            err.contains("JWT/OIDC provider configuration required")
                || err.contains("Secrets policy violation")
        );
    }
}
