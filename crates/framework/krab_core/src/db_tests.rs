#[cfg(test)]
mod tests {
    use crate::db::{
        detect_migration_drift, enforce_migration_governance, record_rollback_rehearsal,
        rollback_to_version, run_versioned_migrations, DbPool, Migration, MigrationFailurePolicy,
        MigrationGovernanceConfig,
    };
    use anyhow::Result;
    use sqlx::postgres::PgPoolOptions;

    // Helper to get a clean DB connection for testing
    // Requires a running Postgres instance.
    // For CI/local dev without DB, these tests will fail if not skipped or mocked.
    // We assume a 'krab_test' database exists for these tests as per CI config.
    async fn get_test_pool() -> Option<DbPool> {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/krab_test".to_string());
        PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .ok()
    }

    async fn clean_test_db(pool: &DbPool) -> Result<()> {
        sqlx::query("DROP TABLE IF EXISTS krab_migration_policy_audit, krab_migration_schema_ownership, krab_migration_rollback_rehearsals, krab_migrations, krab_migration_environment, user_audit_log, user_profiles, users")
            .execute(pool)
            .await?;
        Ok(())
    }

    fn test_users_service_migrations() -> Vec<Migration> {
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

    #[tokio::test]
    async fn test_migration_lifecycle() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!("Skipping test_migration_lifecycle: database not available");
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let migrations = test_users_service_migrations();

        // 1. Run migrations
        let report = run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("migration run failed");

        assert_eq!(report.applied_versions.len(), 7);
        assert_eq!(report.applied_versions, vec![1, 2, 3, 4, 5, 6, 7]);

        // 2. Verify drift detection shows clean state
        let drift = detect_migration_drift(&pool, &migrations)
            .await
            .expect("drift detection failed");

        assert!(drift.missing_versions.is_empty());
        assert!(drift.unexpected_versions.is_empty());
        assert!(drift.checksum_mismatches.is_empty());
    }

    #[tokio::test]
    async fn test_migration_rollback() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!("Skipping test_migration_rollback: database not available");
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let migrations = test_users_service_migrations();

        // 1. Apply all
        run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("migration run failed");

        // 2. Rollback to version 2
        rollback_to_version(&pool, &migrations, 2)
            .await
            .expect("rollback failed");

        // 3. Verify state
        let drift = detect_migration_drift(&pool, &migrations)
            .await
            .expect("drift detection failed");

        // Versions 3, 4, 5 should be missing
        assert!(drift.missing_versions.contains(&3));
        assert!(drift.missing_versions.contains(&4));
        assert!(drift.missing_versions.contains(&5));
        assert!(!drift.missing_versions.contains(&2));
    }

    #[tokio::test]
    async fn test_drift_detection() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!("Skipping test_drift_detection: database not available");
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let mut migrations = test_users_service_migrations();

        // 1. Apply initial set
        run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("migration run failed");

        // 2. Simulate drift: add a new migration definition but don't apply it
        migrations.push(Migration {
            version: 999,
            name: "drift_test",
            sql: "SELECT 1",
            rollback_sql: None,
            critical: false,
            destructive: false,
        });

        let drift = detect_migration_drift(&pool, &migrations)
            .await
            .expect("drift detection failed");

        assert!(drift.missing_versions.contains(&999));
    }

    #[tokio::test]
    async fn test_governance_release_requires_rehearsal_artifact() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!(
                    "Skipping test_governance_release_requires_rehearsal_artifact: database not available"
                );
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let cfg = MigrationGovernanceConfig {
            service_name: "service_users".to_string(),
            environment: "staging".to_string(),
            allow_apply: true,
            release_environments: vec!["staging".to_string(), "prod".to_string()],
            require_rollback_rehearsal_in_release: true,
            drift_tolerance_threshold: 0,
        };

        let result = enforce_migration_governance(&pool, &cfg).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_governance_release_passes_with_rehearsal_artifact() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!(
                    "Skipping test_governance_release_passes_with_rehearsal_artifact: database not available"
                );
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        record_rollback_rehearsal(
            &pool,
            "service_users",
            "staging",
            3,
            "s3://evidence/rollback-rehearsal-2026-02-27.json",
            true,
        )
        .await
        .expect("failed to record rollback rehearsal");

        let cfg = MigrationGovernanceConfig {
            service_name: "service_users".to_string(),
            environment: "staging".to_string(),
            allow_apply: true,
            release_environments: vec!["staging".to_string(), "prod".to_string()],
            require_rollback_rehearsal_in_release: true,
            drift_tolerance_threshold: 0,
        };

        enforce_migration_governance(&pool, &cfg)
            .await
            .expect("governance should pass with rehearsal artifact");
    }

    #[tokio::test]
    async fn test_legacy_checksum_rows_are_rewritten_not_flagged() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!(
                    "Skipping test_legacy_checksum_rows_are_rewritten_not_flagged: database not available"
                );
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let migrations = test_users_service_migrations();
        run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("migration run failed");

        // Simulate a row written before the SHA-256 switch: same SQL, legacy
        // DefaultHasher checksum format.
        let legacy = crate::db::postgres::legacy_checksum(migrations[0].sql);
        sqlx::query("UPDATE krab_migrations SET checksum = $1 WHERE version = $2")
            .bind(&legacy)
            .bind(migrations[0].version)
            .execute(&pool)
            .await
            .expect("failed to plant legacy checksum");

        // Drift detection must upgrade the row in place, not flag it.
        let drift = detect_migration_drift(&pool, &migrations)
            .await
            .expect("drift detection failed");
        assert!(
            drift.checksum_mismatches.is_empty(),
            "legacy checksum must be upgraded, not reported as drift: {:?}",
            drift.checksum_mismatches
        );

        let stored: String =
            sqlx::query_scalar("SELECT checksum FROM krab_migrations WHERE version = $1")
                .bind(migrations[0].version)
                .fetch_one(&pool)
                .await
                .expect("failed to read back checksum");
        assert_eq!(
            stored,
            crate::db::postgres::checksum(migrations[0].sql),
            "row must be rewritten to the SHA-256 checksum"
        );

        // A re-run of the migrations must also accept-and-rewrite.
        sqlx::query("UPDATE krab_migrations SET checksum = $1 WHERE version = $2")
            .bind(&legacy)
            .bind(migrations[0].version)
            .execute(&pool)
            .await
            .expect("failed to re-plant legacy checksum");

        let report = run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("re-run over a legacy checksum row must not fail");
        assert!(report.skipped_versions.contains(&migrations[0].version));

        let stored: String =
            sqlx::query_scalar("SELECT checksum FROM krab_migrations WHERE version = $1")
                .bind(migrations[0].version)
                .fetch_one(&pool)
                .await
                .expect("failed to read back checksum");
        assert_eq!(stored, crate::db::postgres::checksum(migrations[0].sql));
    }

    #[tokio::test]
    async fn test_true_checksum_mismatch_is_still_flagged() {
        let pool = match get_test_pool().await {
            Some(p) => p,
            None => {
                println!(
                    "Skipping test_true_checksum_mismatch_is_still_flagged: database not available"
                );
                return;
            }
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        let migrations = test_users_service_migrations();
        run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("migration run failed");

        // Neither the SHA-256 nor the legacy checksum of this SQL: real drift.
        sqlx::query(
            "UPDATE krab_migrations SET checksum = 'not-any-known-format' WHERE version = $1",
        )
        .bind(migrations[0].version)
        .execute(&pool)
        .await
        .expect("failed to plant drifted checksum");

        let drift = detect_migration_drift(&pool, &migrations)
            .await
            .expect("drift detection failed");
        assert!(drift.checksum_mismatches.contains(&migrations[0].version));

        let rerun =
            run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt).await;
        assert!(
            rerun.is_err(),
            "a genuine checksum mismatch must still fail the migration run"
        );
    }

    #[test]
    fn test_enforce_drift_policy() {
        use crate::db::{enforce_drift_policy, MigrationDriftReport};

        let report_clean = MigrationDriftReport::default();
        assert!(enforce_drift_policy(&report_clean, 0).is_ok());

        let report_unexpected = MigrationDriftReport {
            unexpected_versions: vec![999],
            ..Default::default()
        };
        // Allowed threshold is 1
        assert!(enforce_drift_policy(&report_unexpected, 1).is_ok());
        // Threshold 0 should fail
        assert!(enforce_drift_policy(&report_unexpected, 0).is_err());

        // Mismatches or missing versions should always fail
        let report_missing = MigrationDriftReport {
            missing_versions: vec![1],
            ..Default::default()
        };
        assert!(enforce_drift_policy(&report_missing, 1).is_err());
    }
}
