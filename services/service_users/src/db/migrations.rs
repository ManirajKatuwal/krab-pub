use anyhow::{Context as _, Result};
use krab_core::db::{
    detect_migration_drift, enforce_migration_governance, enforce_promotion_policy,
    migration_failure_policy_from_env, run_versioned_migrations, DbPool, Migration,
    MigrationGovernanceConfig, PromotionConfig,
};
use sqlx::SqlitePool;
use tracing::info;

pub(crate) fn users_service_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "create_users",
            sql: "CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, email TEXT NOT NULL UNIQUE, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            rollback_sql: Some("DROP TABLE IF EXISTS users"),
            critical: true,
            destructive: false,
        },
        Migration {
            version: 2,
            name: "create_users_created_at_index",
            sql: "CREATE INDEX IF NOT EXISTS idx_users_created_at ON users(created_at)",
            rollback_sql: Some("DROP INDEX IF EXISTS idx_users_created_at"),
            critical: false,
            destructive: false,
        },
        Migration {
            version: 3,
            name: "create_user_profiles",
            sql: "CREATE TABLE IF NOT EXISTS user_profiles (user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE, display_name TEXT, bio TEXT, avatar_url TEXT, updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            rollback_sql: Some("DROP TABLE IF EXISTS user_profiles"),
            critical: false,
            destructive: false,
        },
        Migration {
            version: 4,
            name: "create_user_audit_log",
            sql: "CREATE TABLE IF NOT EXISTS user_audit_log (id BIGSERIAL PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, action TEXT NOT NULL, actor_sub TEXT, created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
            rollback_sql: Some("DROP TABLE IF EXISTS user_audit_log"),
            critical: false,
            destructive: false,
        },
        Migration {
            version: 5,
            name: "create_user_audit_log_created_at_index",
            sql: "CREATE INDEX IF NOT EXISTS idx_user_audit_log_created_at ON user_audit_log(created_at)",
            rollback_sql: Some("DROP INDEX IF EXISTS idx_user_audit_log_created_at"),
            critical: false,
            destructive: false,
        },
        Migration {
            version: 6,
            name: "add_tenant_id_to_users",
            sql: "ALTER TABLE users ADD COLUMN IF NOT EXISTS tenant_id TEXT",
            rollback_sql: Some("ALTER TABLE users DROP COLUMN IF EXISTS tenant_id"),
            critical: true,
            destructive: false,
        },
        Migration {
            version: 7,
            name: "create_users_tenant_id_index",
            sql: "CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON users(tenant_id)",
            rollback_sql: Some("DROP INDEX IF EXISTS idx_users_tenant_id"),
            critical: false,
            destructive: false,
        },
    ]
}

pub(crate) async fn run_postgres_migration_lifecycle(pool: &DbPool) -> Result<()> {
    let promotion = PromotionConfig::from_env();
    enforce_promotion_policy(pool, &promotion)
        .await
        .context("failed to enforce migration promotion policy")?;

    let governance = MigrationGovernanceConfig::from_env();
    enforce_migration_governance(pool, &governance)
        .await
        .context("failed to enforce migration governance policy")?;

    anyhow::ensure!(
        promotion.allow_apply,
        "DB_MIGRATION_ALLOW_APPLY is false; refusing to run automatic migrations"
    );

    let migrations = users_service_migrations();
    let report = run_versioned_migrations(pool, &migrations, migration_failure_policy_from_env())
        .await
        .context("failed to run users migrations")?;
    info!(
        applied = ?report.applied_versions,
        skipped = ?report.skipped_versions,
        "users_migrations_applied"
    );

    let drift = detect_migration_drift(pool, &migrations)
        .await
        .context("failed to detect users migration drift")?;
    info!(
        missing = ?drift.missing_versions,
        unexpected = ?drift.unexpected_versions,
        checksum_mismatches = ?drift.checksum_mismatches,
        environment = %promotion.environment,
        "users_migration_drift_report"
    );

    Ok(())
}

pub(crate) async fn bootstrap_sqlite_users_schema(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            email TEXT NOT NULL UNIQUE,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            tenant_id TEXT NULL
        )",
    )
    .execute(pool)
    .await
    .context("failed to bootstrap sqlite users schema")?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_users_created_at ON users(created_at)")
        .execute(pool)
        .await
        .context("failed to create sqlite users index")?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON users(tenant_id)")
        .execute(pool)
        .await
        .context("failed to create sqlite tenant index")?;

    info!("sqlite_users_schema_bootstrapped");
    Ok(())
}
