use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Row};
use sqlx::{Pool, Postgres};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

fn bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

fn csv_env(name: &str, default: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

pub type DbPool = Pool<Postgres>;

#[derive(Debug, Clone)]
pub struct MigrationRecord {
    pub version: i64,
    pub name: String,
    pub checksum: String,
}

impl<'r, R> sqlx::FromRow<'r, R> for MigrationRecord
where
    R: Row,
    &'r str: sqlx::ColumnIndex<R>,
    i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    fn from_row(row: &'r R) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            version: row.try_get("version")?,
            name: row.try_get("name")?,
            checksum: row.try_get("checksum")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct UserModel {
    pub id: String,
    pub username: String,
    pub email: String,
    pub tenant_id: Option<String>,
    pub created_at: sqlx::types::time::OffsetDateTime,
    pub updated_at: sqlx::types::time::OffsetDateTime,
}

impl<'r, R> sqlx::FromRow<'r, R> for UserModel
where
    R: Row,
    &'r str: sqlx::ColumnIndex<R>,
    String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    sqlx::types::time::OffsetDateTime: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    fn from_row(row: &'r R) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            email: row.try_get("email")?,
            tenant_id: row.try_get("tenant_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
    pub rollback_sql: Option<&'static str>,
    pub critical: bool,
    pub destructive: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum MigrationFailurePolicy {
    Halt,
    ContinueNonCritical,
}

pub fn migration_failure_policy_from_env() -> MigrationFailurePolicy {
    match std::env::var("DB_MIGRATION_FAILURE_POLICY") {
        Ok(value) if value.eq_ignore_ascii_case("continue_non_critical") => {
            MigrationFailurePolicy::ContinueNonCritical
        }
        _ => MigrationFailurePolicy::Halt,
    }
}

#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    pub applied_versions: Vec<i64>,
    pub skipped_versions: Vec<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct MigrationDriftReport {
    pub missing_versions: Vec<i64>,
    pub unexpected_versions: Vec<i64>,
    pub checksum_mismatches: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct PromotionConfig {
    pub environment: String,
    pub allow_apply: bool,
}

#[derive(Debug, Clone)]
pub struct MigrationGovernanceConfig {
    pub service_name: String,
    pub environment: String,
    pub allow_apply: bool,
    pub release_environments: Vec<String>,
    pub require_rollback_rehearsal_in_release: bool,
    pub drift_tolerance_threshold: usize,
}

impl MigrationGovernanceConfig {
    pub fn from_env() -> Self {
        Self {
            service_name: std::env::var("KRAB_SERVICE_NAME")
                .unwrap_or_else(|_| "unknown".to_string()),
            environment: std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string()),
            allow_apply: bool_env("DB_MIGRATION_ALLOW_APPLY", true),
            release_environments: csv_env("DB_MIGRATION_RELEASE_ENVIRONMENTS", "staging,prod"),
            require_rollback_rehearsal_in_release: bool_env(
                "DB_MIGRATION_REQUIRE_REHEARSAL_IN_RELEASE",
                true,
            ),
            drift_tolerance_threshold: std::env::var("DB_MIGRATION_DRIFT_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }
}

impl PromotionConfig {
    pub fn from_env() -> Self {
        let environment = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
        let allow_apply = std::env::var("DB_MIGRATION_ALLOW_APPLY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);
        Self {
            environment,
            allow_apply,
        }
    }
}

/// Create the rehearsal ledger if absent. Shared by the recording path and the
/// governance check so a fresh database yields the governance verdict
/// ("missing successful rollback rehearsal") rather than a raw
/// `relation does not exist` error.
async fn ensure_rehearsal_table(pool: &DbPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS krab_migration_rollback_rehearsals (
            id BIGSERIAL PRIMARY KEY,
            service_name TEXT NOT NULL,
            environment TEXT NOT NULL,
            rollback_target BIGINT NOT NULL,
            artifact_uri TEXT NOT NULL,
            succeeded BOOLEAN NOT NULL,
            executed_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_rollback_rehearsal(
    pool: &DbPool,
    service_name: &str,
    environment: &str,
    rollback_target: i64,
    artifact_uri: &str,
    succeeded: bool,
) -> Result<()> {
    ensure_rehearsal_table(pool).await?;

    sqlx::query(
        "INSERT INTO krab_migration_rollback_rehearsals
            (service_name, environment, rollback_target, artifact_uri, succeeded)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(service_name)
    .bind(environment)
    .bind(rollback_target)
    .bind(artifact_uri)
    .bind(succeeded)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn enforce_migration_governance(
    pool: &DbPool,
    cfg: &MigrationGovernanceConfig,
) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS krab_migration_schema_ownership (
            service_name TEXT PRIMARY KEY,
            environment TEXT NOT NULL,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS krab_migration_policy_audit (
            id BIGSERIAL PRIMARY KEY,
            service_name TEXT NOT NULL,
            environment TEXT NOT NULL,
            policy_name TEXT NOT NULL,
            decision TEXT NOT NULL,
            detail JSONB,
            recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO krab_migration_schema_ownership (service_name, environment)
         VALUES ($1, $2)
         ON CONFLICT (service_name) DO UPDATE SET environment = EXCLUDED.environment, updated_at = now()",
    )
    .bind(&cfg.service_name)
    .bind(&cfg.environment)
    .execute(pool)
    .await?;

    let is_release = cfg
        .release_environments
        .iter()
        .any(|e| e.eq_ignore_ascii_case(&cfg.environment));

    if is_release && cfg.require_rollback_rehearsal_in_release {
        ensure_rehearsal_table(pool).await?;
        let has_rehearsal: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1
                FROM krab_migration_rollback_rehearsals
                WHERE service_name = $1
                  AND succeeded = true
            )",
        )
        .bind(&cfg.service_name)
        .fetch_optional(pool)
        .await?
        .unwrap_or(false);

        if !has_rehearsal {
            anyhow::bail!(
                "migration governance violation: missing successful rollback rehearsal artifact for service '{}' in release environment '{}'",
                cfg.service_name,
                cfg.environment
            );
        }
    }

    let decision = if cfg.allow_apply { "allow" } else { "deny" };
    let detail = serde_json::json!({
        "is_release_environment": is_release,
        "require_rollback_rehearsal_in_release": cfg.require_rollback_rehearsal_in_release,
        "release_environments": cfg.release_environments,
    });

    sqlx::query(
        "INSERT INTO krab_migration_policy_audit (service_name, environment, policy_name, decision, detail)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&cfg.service_name)
    .bind(&cfg.environment)
    .bind("migration_governance")
    .bind(decision)
    .bind(detail)
    .execute(pool)
    .await?;

    Ok(())
}

pub fn enforce_drift_policy(report: &MigrationDriftReport, max_unexpected: usize) -> Result<()> {
    if !report.missing_versions.is_empty() {
        anyhow::bail!(
            "drift policy violation: database is missing expected migrations: {:?}",
            report.missing_versions
        );
    }
    if !report.checksum_mismatches.is_empty() {
        anyhow::bail!(
            "drift policy violation: checksum mismatches detected (potential manual DB edits): {:?}",
            report.checksum_mismatches
        );
    }
    if report.unexpected_versions.len() > max_unexpected {
        anyhow::bail!(
            "drift policy violation: {} unexpected versions exist, exceeding threshold {}",
            report.unexpected_versions.len(),
            max_unexpected
        );
    }
    Ok(())
}

#[derive(Clone)]
pub struct DbConfig {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: Duration,
    pub max_lifetime: Duration,
    pub idle_timeout: Duration,
    pub connect_retries: u32,
    pub connect_retry_delay: Duration,
}

/// Manual impl, not derived: `url` is a full `DATABASE_URL` and usually
/// carries the password, so a `{:?}` in error context or a diagnostic dump
/// must never print it verbatim.
impl std::fmt::Debug for DbConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbConfig")
            .field("url", &redact_db_url(&self.url))
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .field("max_lifetime", &self.max_lifetime)
            .field("idle_timeout", &self.idle_timeout)
            .field("connect_retries", &self.connect_retries)
            .field("connect_retry_delay", &self.connect_retry_delay)
            .finish()
    }
}

/// Mask the userinfo section of a connection URL (`scheme://user:pass@host`).
/// Anything between `//` and the last `@` before the host is replaced, so
/// neither username nor password survives into logs.
fn redact_db_url(url: &str) -> String {
    let Some(scheme_end) = url.find("//") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 2;
    let authority_end = url[authority_start..]
        .find('/')
        .map(|i| authority_start + i)
        .unwrap_or(url.len());
    match url[authority_start..authority_end].rfind('@') {
        Some(at) => format!(
            "{}***@{}",
            &url[..authority_start],
            &url[authority_start + at + 1..]
        ),
        None => url.to_string(),
    }
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            url: "postgres://postgres@localhost:5432/krab".to_string(),
            max_connections: 10,
            min_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            max_lifetime: Duration::from_secs(30 * 60),
            idle_timeout: Duration::from_secs(10 * 60),
            connect_retries: 5,
            connect_retry_delay: Duration::from_millis(750),
        }
    }
}

impl DbConfig {
    pub fn validate_security(&self) -> Result<()> {
        let env = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
        let is_non_local = !env.eq_ignore_ascii_case("local") && !env.eq_ignore_ascii_case("dev");

        if !is_non_local {
            return Ok(());
        }

        let url = self.url.trim();
        if url.is_empty() {
            anyhow::bail!("DATABASE_URL must be set in '{}' environment", env);
        }

        if url.contains("postgres:password@") {
            anyhow::bail!(
                "insecure default database credentials detected in '{}' environment; rotate DATABASE_URL credentials",
                env
            );
        }

        Ok(())
    }

    /// Load pool configuration from the environment.
    ///
    /// A missing `DATABASE_URL` falls back to `default_url` (the dev
    /// convenience). An **error** resolving it — an unreadable or empty
    /// `DATABASE_URL_FILE`, an unresolved `DATABASE_URL_VAULT_REF` — is
    /// returned, never swallowed: a broken secret source must fail startup
    /// rather than silently connect to the localhost default.
    pub fn from_env(default_url: &str) -> Result<Self> {
        fn parse_u32(name: &str, default: u32) -> u32 {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(default)
        }

        let mut cfg = Self::default();
        cfg.url = crate::config::read_env_or_file("DATABASE_URL")
            .context("failed to resolve DATABASE_URL from its configured secret source")?
            .unwrap_or_else(|| default_url.to_string());
        cfg.max_connections = parse_u32("DB_MAX_CONNECTIONS", cfg.max_connections);
        cfg.min_connections = parse_u32("DB_MIN_CONNECTIONS", cfg.min_connections);
        cfg.connect_retries = parse_u32("DB_CONNECT_RETRIES", cfg.connect_retries);
        cfg.connect_retry_delay = Duration::from_millis(parse_u32(
            "DB_CONNECT_RETRY_DELAY_MS",
            cfg.connect_retry_delay.as_millis() as u32,
        ) as u64);
        cfg.acquire_timeout = Duration::from_secs(parse_u32(
            "DB_ACQUIRE_TIMEOUT_SECS",
            cfg.acquire_timeout.as_secs() as u32,
        ) as u64);
        cfg.max_lifetime = Duration::from_secs(parse_u32(
            "DB_MAX_LIFETIME_SECS",
            cfg.max_lifetime.as_secs() as u32,
        ) as u64);
        cfg.idle_timeout = Duration::from_secs(parse_u32(
            "DB_IDLE_TIMEOUT_SECS",
            cfg.idle_timeout.as_secs() as u32,
        ) as u64);
        Ok(cfg)
    }
}

pub async fn connect(database_url: &str) -> Result<DbPool> {
    connect_with_config(&DbConfig {
        url: database_url.to_string(),
        ..DbConfig::default()
    })
    .await
}

pub async fn connect_with_config(cfg: &DbConfig) -> Result<DbPool> {
    cfg.validate_security()?;

    let mut attempt = 0;
    loop {
        let result = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(cfg.min_connections)
            .acquire_timeout(cfg.acquire_timeout)
            .max_lifetime(cfg.max_lifetime)
            .idle_timeout(cfg.idle_timeout)
            .connect(&cfg.url)
            .await;

        match result {
            Ok(pool) => return Ok(pool),
            Err(e) => {
                attempt += 1;
                if attempt >= cfg.connect_retries {
                    return Err(e.into());
                }

                // Exponential backoff + deterministic jitter to reduce thundering herd behavior.
                let exp = (attempt.saturating_sub(1)).min(6); // cap at 2^6 growth
                let multiplier = 1u64 << exp;
                let base_ms = cfg.connect_retry_delay.as_millis() as u64;
                let backoff_ms = base_ms.saturating_mul(multiplier).min(30_000);

                let mut hasher = DefaultHasher::new();
                cfg.url.hash(&mut hasher);
                attempt.hash(&mut hasher);
                let jitter_ms = hasher.finish() % 250;

                sleep(Duration::from_millis(backoff_ms.saturating_add(jitter_ms))).await;
            }
        }
    }
}

pub async fn run_migrations(pool: &DbPool) -> Result<()> {
    let _ = run_versioned_migrations(
        pool,
        &[Migration {
            version: 1,
            name: "bootstrap_migration_table",
            sql: "CREATE TABLE IF NOT EXISTS _krab_migrations (id SERIAL PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now(), name TEXT NOT NULL UNIQUE)",
            rollback_sql: None,
            critical: true,
            destructive: false,
        }],
        MigrationFailurePolicy::Halt,
    )
    .await?;

    Ok(())
}

/// SHA-256 checksum of a migration's SQL, hex-encoded (64 characters).
///
/// Checksums are persisted in `krab_migrations` and compared across process
/// and toolchain boundaries, so they must be computed by a stable, specified
/// hash. They used to come from std's `DefaultHasher`, whose output is
/// documented as unstable across Rust releases — a toolchain bump would have
/// flagged every previously applied migration as drifted.
pub(crate) fn checksum(input: &str) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The pre-0.2.0 checksum format: std's `DefaultHasher`, 16 hex characters.
///
/// Only used to recognise rows written by earlier versions so they can be
/// upgraded in place. This value is reproducible only while the running
/// toolchain's `DefaultHasher` matches the one that wrote the row — which is
/// exactly the upgrade window: run any 0.2.0 migration pass (or drift check)
/// before bumping the Rust toolchain and every legacy row is rewritten to
/// SHA-256; afterwards the legacy hash is never consulted for that row again.
pub(crate) fn legacy_checksum(input: &str) -> String {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChecksumStatus {
    /// Stored value matches the current SHA-256 checksum.
    Current,
    /// Stored value matches the legacy `DefaultHasher` checksum of the same
    /// SQL — the row predates the SHA-256 switch and should be rewritten.
    Legacy,
    /// Stored value matches neither: real drift.
    Mismatch,
}

pub(crate) fn classify_checksum(stored: &str, sql: &str) -> ChecksumStatus {
    if stored == checksum(sql) {
        ChecksumStatus::Current
    } else if stored == legacy_checksum(sql) {
        ChecksumStatus::Legacy
    } else {
        ChecksumStatus::Mismatch
    }
}

/// Rewrite a legacy-format checksum row to the current SHA-256 value.
async fn upgrade_legacy_checksum_row(pool: &DbPool, version: i64, sql: &str) -> Result<()> {
    sqlx::query("UPDATE krab_migrations SET checksum = $1 WHERE version = $2")
        .bind(checksum(sql))
        .bind(version)
        .execute(pool)
        .await?;
    warn!(
        version,
        "migration_checksum_upgraded_from_legacy_default_hasher_to_sha256"
    );
    Ok(())
}

pub async fn enforce_promotion_policy(pool: &DbPool, cfg: &PromotionConfig) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS krab_migration_environment (environment TEXT PRIMARY KEY, updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )
    .execute(pool)
    .await?;

    // Determine target environment from config
    let target_env = cfg.environment.as_str();

    let rows = sqlx::query_scalar::<_, String>(
        "SELECT environment FROM krab_migration_environment ORDER BY updated_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    if let Some(previous) = rows {
        // Enforce promotion order: local -> dev -> staging -> prod
        let order = ["local", "dev", "staging", "prod"];
        let prev_idx = order.iter().position(|&e| e == previous).unwrap_or(0);
        let target_idx = order.iter().position(|&e| e == target_env).unwrap_or(0);

        if target_idx < prev_idx {
            anyhow::bail!(
                "migration promotion violation: cannot promote backwards from '{}' to '{}'",
                previous,
                target_env
            );
        }

        if previous != target_env && target_idx > prev_idx + 1 {
            warn!(
                "migration promotion skipped a stage: '{}' to '{}'",
                previous, target_env
            );
        }

        if previous != target_env {
            sqlx::query(
                "INSERT INTO krab_migration_environment (environment) VALUES ($1) ON CONFLICT (environment) DO UPDATE SET updated_at = now()",
            )
            .bind(target_env)
            .execute(pool)
            .await?;
        }
    } else {
        sqlx::query(
            "INSERT INTO krab_migration_environment (environment) VALUES ($1) ON CONFLICT (environment) DO UPDATE SET updated_at = now()",
        )
        .bind(target_env)
        .execute(pool)
        .await?;
    }

    Ok(())
}

pub async fn detect_migration_drift(
    pool: &DbPool,
    migrations: &[Migration],
) -> Result<MigrationDriftReport> {
    let mut report = MigrationDriftReport::default();

    let rows = sqlx::query_as::<_, MigrationRecord>(
        "SELECT version, name, checksum FROM krab_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await?;

    let expected_sql: std::collections::BTreeMap<i64, &str> =
        migrations.iter().map(|m| (m.version, m.sql)).collect();
    let applied: std::collections::BTreeMap<i64, String> = rows
        .iter()
        .map(|m| (m.version, m.checksum.clone()))
        .collect();

    for version in expected_sql.keys() {
        if !applied.contains_key(version) {
            report.missing_versions.push(*version);
        }
    }

    for version in applied.keys() {
        if !expected_sql.contains_key(version) {
            report.unexpected_versions.push(*version);
        }
    }

    for (version, sql) in &expected_sql {
        if let Some(applied_checksum) = applied.get(version) {
            match classify_checksum(applied_checksum, sql) {
                ChecksumStatus::Current => {}
                ChecksumStatus::Legacy => {
                    // Row written before the SHA-256 switch and the SQL is
                    // unchanged: upgrade in place instead of flagging drift.
                    upgrade_legacy_checksum_row(pool, *version, sql).await?;
                }
                ChecksumStatus::Mismatch => report.checksum_mismatches.push(*version),
            }
        }
    }

    Ok(report)
}

pub async fn rollback_to_version(
    pool: &DbPool,
    migrations: &[Migration],
    target_version: i64,
) -> Result<()> {
    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);
    sorted.reverse();

    for migration in sorted {
        if migration.version <= target_version {
            continue;
        }

        let exists: Option<i64> =
            sqlx::query_scalar("SELECT version FROM krab_migrations WHERE version = $1")
                .bind(migration.version)
                .fetch_optional(pool)
                .await?;

        if exists.is_none() {
            continue;
        }

        let rollback_sql = migration.rollback_sql.ok_or_else(|| {
            anyhow::anyhow!(
                "rollback sql missing for version {} ({})",
                migration.version,
                migration.name
            )
        })?;

        let mut tx = pool.begin().await?;
        sqlx::query(rollback_sql).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM krab_migrations WHERE version = $1")
            .bind(migration.version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

    Ok(())
}

pub async fn run_preflight_checks(pool: &DbPool) -> Result<()> {
    // 1. Lock acquisition test
    // Use an advisory lock to ensure we can acquire locks
    let lock_id = 999999;
    let lock_acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(lock_id)
        .fetch_one(pool)
        .await?;

    if !lock_acquired {
        anyhow::bail!(
            "preflight check failed: could not acquire advisory lock {}",
            lock_id
        );
    }

    // Release the lock and verify success.
    let lock_released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(lock_id)
        .fetch_one(pool)
        .await?;

    if !lock_released {
        anyhow::bail!(
            "preflight check failed: advisory lock {} was not released",
            lock_id
        );
    }

    // 2. Dependency checks (e.g. ensure extensions can be created if needed,
    // or check for required extensions like 'uuid-ossp' or 'pgcrypto')
    // For now, we just check connection is healthy and we are superuser or owner
    let role: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(pool)
        .await?;

    // 3. Impact warnings
    // Warn if running as superuser in production
    if role == "postgres" && std::env::var("KRAB_ENVIRONMENT").unwrap_or_default() == "prod" {
        warn!("running migrations as superuser 'postgres' in production is not recommended");
    }

    Ok(())
}

/// Advisory-lock key serializing migration runs across processes: ASCII
/// `krab_mig` as an `i64`. Session-level, so a migrator that crashes mid-run
/// releases it when its connection dies.
const MIGRATION_ADVISORY_LOCK_KEY: i64 = 0x6b72_6162_5f6d_6967;

pub async fn run_versioned_migrations(
    pool: &DbPool,
    migrations: &[Migration],
    failure_policy: MigrationFailurePolicy,
) -> Result<MigrationReport> {
    // Two replicas booting through a rolling deploy both reach this function;
    // without cross-process serialization both observe version N as unapplied
    // and both execute its DDL — one commits, the other crashes on the
    // `krab_migrations` primary key. Hold a session-level advisory lock on a
    // dedicated connection for the whole run so exactly one migrator proceeds
    // at a time; the loser waits, then skips the now-applied versions.
    let mut lock_conn = pool
        .acquire()
        .await
        .context("failed to acquire a connection for the migration advisory lock")?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_ADVISORY_LOCK_KEY)
        .execute(&mut *lock_conn)
        .await
        .context("failed to take the migration advisory lock")?;

    let result = run_versioned_migrations_locked(pool, migrations, failure_policy).await;

    let unlocked: std::result::Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *lock_conn)
            .await;
    if !matches!(unlocked, Ok(true)) {
        // Advisory locks belong to the session, and pooled sessions are
        // reused: a connection returned while still holding the lock would
        // block every future migrator. Close it so Postgres releases the lock.
        let _ = lock_conn.detach().close().await;
    }

    result
}

async fn run_versioned_migrations_locked(
    pool: &DbPool,
    migrations: &[Migration],
    failure_policy: MigrationFailurePolicy,
) -> Result<MigrationReport> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS krab_migrations (version BIGINT PRIMARY KEY, name TEXT NOT NULL, checksum TEXT NOT NULL, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )
    .execute(pool)
    .await?;

    let mut report = MigrationReport::default();

    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);

    for migration in sorted {
        let expected_checksum = checksum(migration.sql);

        // Validate destructive safety before attempting SQL execution.
        if migration.destructive && migration.rollback_sql.is_none() {
            anyhow::bail!(
                "destructive migration (version {}, name {}) missing mandatory rollback_sql",
                migration.version,
                migration.name
            );
        }

        let existing = sqlx::query_as::<_, MigrationRecord>(
            "SELECT version, name, checksum FROM krab_migrations WHERE version = $1",
        )
        .bind(migration.version)
        .fetch_optional(pool)
        .await?;

        if let Some(record) = existing {
            match classify_checksum(&record.checksum, migration.sql) {
                ChecksumStatus::Current => {}
                ChecksumStatus::Legacy => {
                    // Same SQL, pre-SHA-256 checksum format: rewrite the row
                    // rather than failing the run.
                    upgrade_legacy_checksum_row(pool, migration.version, migration.sql).await?;
                }
                ChecksumStatus::Mismatch => {
                    anyhow::bail!(
                        "migration checksum mismatch for version {}: expected {}, found {}",
                        migration.version,
                        expected_checksum,
                        record.checksum
                    );
                }
            }
            report.skipped_versions.push(migration.version);
            continue;
        }

        let mut tx = pool.begin().await?;
        let apply_result = sqlx::query(migration.sql).execute(&mut *tx).await;

        if let Err(error) = apply_result {
            let _ = tx.rollback().await;
            match failure_policy {
                MigrationFailurePolicy::Halt => {
                    anyhow::bail!(
                        "critical migration failed (version {}, name {}): {}",
                        migration.version,
                        migration.name,
                        error
                    );
                }
                MigrationFailurePolicy::ContinueNonCritical if !migration.critical => {
                    warn!(
                        version = migration.version,
                        name = migration.name,
                        error = %error,
                        "non_critical_migration_failed"
                    );
                    continue;
                }
                MigrationFailurePolicy::ContinueNonCritical => {
                    anyhow::bail!(
                        "critical migration failed under continue policy (version {}, name {}): {}",
                        migration.version,
                        migration.name,
                        error
                    );
                }
            }
        }

        sqlx::query("INSERT INTO krab_migrations (version, name, checksum) VALUES ($1, $2, $3)")
            .bind(migration.version)
            .bind(migration.name)
            .bind(expected_checksum)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        report.applied_versions.push(migration.version);
    }

    Ok(report)
}

#[cfg(test)]
mod redaction_tests {
    use super::{redact_db_url, DbConfig};

    #[test]
    fn debug_output_never_contains_the_password() {
        let cfg = DbConfig {
            url: "postgres://svc_user:s3cret@db.internal:5432/krab".to_string(),
            ..DbConfig::default()
        };
        let printed = format!("{cfg:?}");
        assert!(!printed.contains("s3cret"));
        assert!(!printed.contains("svc_user"));
        assert!(printed.contains("db.internal:5432/krab"));
    }

    #[test]
    fn redaction_handles_urls_without_credentials_or_scheme() {
        assert_eq!(
            redact_db_url("postgres://localhost:5432/krab"),
            "postgres://localhost:5432/krab"
        );
        assert_eq!(redact_db_url("not-a-url"), "not-a-url");
        assert_eq!(
            redact_db_url("postgres://u:p@ss@host/db"),
            "postgres://***@host/db"
        );
    }
}

#[cfg(test)]
mod checksum_tests {
    use super::{checksum, classify_checksum, legacy_checksum, ChecksumStatus};

    #[test]
    fn checksum_is_stable_sha256_hex() {
        // Known SHA-256 digest: independently verifiable, and pins the
        // implementation to a specified hash rather than std's unstable
        // DefaultHasher.
        assert_eq!(
            checksum("SELECT 1"),
            "e004ebd5b5532a4b85984a62f8ad48a81aa3460c1ca07701f386135d72cdecf5"
        );
        assert_eq!(checksum("SELECT 1").len(), 64);
        assert_ne!(checksum("SELECT 1"), checksum("SELECT 2"));
    }

    #[test]
    fn classify_checksum_distinguishes_current_legacy_and_drift() {
        let sql = "CREATE TABLE t (id BIGINT PRIMARY KEY)";

        assert_eq!(
            classify_checksum(&checksum(sql), sql),
            ChecksumStatus::Current
        );
        assert_eq!(
            classify_checksum(&legacy_checksum(sql), sql),
            ChecksumStatus::Legacy
        );
        assert_eq!(
            classify_checksum("0123456789abcdef", sql),
            ChecksumStatus::Mismatch
        );
        // A legacy checksum of DIFFERENT sql is drift, not an upgrade case.
        assert_eq!(
            classify_checksum(&legacy_checksum("SELECT other"), sql),
            ChecksumStatus::Mismatch
        );
    }
}

#[cfg(test)]
mod db_config_tests {
    use super::DbConfig;
    use serial_test::serial;

    fn clear_db_url_env() {
        for key in [
            "DATABASE_URL",
            "DATABASE_URL_FILE",
            "DATABASE_URL_VAULT_REF",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    #[serial]
    fn missing_database_url_still_falls_back_to_default_in_dev() {
        clear_db_url_env();

        let cfg = DbConfig::from_env("postgres://localhost:5432/dev_default")
            .expect("an unset DATABASE_URL is not an error");
        assert_eq!(cfg.url, "postgres://localhost:5432/dev_default");
    }

    #[test]
    #[serial]
    fn broken_database_url_file_fails_instead_of_defaulting() {
        clear_db_url_env();
        std::env::set_var(
            "DATABASE_URL_FILE",
            "definitely/not/a/real/path/database_url.txt",
        );

        let err = DbConfig::from_env("postgres://localhost:5432/dev_default")
            .expect_err("an unreadable DATABASE_URL_FILE must fail, not fall back");
        let message = format!("{err:#}");
        assert!(
            message.contains("DATABASE_URL"),
            "error should name the failing variable: {message}"
        );

        std::env::remove_var("DATABASE_URL_FILE");
    }

    #[test]
    #[serial]
    fn unresolved_database_url_vault_ref_fails_instead_of_defaulting() {
        clear_db_url_env();
        std::env::set_var("DATABASE_URL_VAULT_REF", "vault://kv/krab/database-url");

        let err = DbConfig::from_env("postgres://localhost:5432/dev_default")
            .expect_err("an unresolved DATABASE_URL_VAULT_REF must fail, not fall back");
        let message = format!("{err:#}");
        assert!(
            message.contains("DATABASE_URL_VAULT_REF"),
            "error should name the failing variable: {message}"
        );

        std::env::remove_var("DATABASE_URL_VAULT_REF");
    }
}
