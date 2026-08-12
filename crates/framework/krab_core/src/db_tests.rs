#[cfg(test)]
mod tests {
    use crate::db::{
        detect_migration_drift, enforce_migration_governance, enforce_promotion_policy,
        record_rollback_rehearsal, rollback_to_version, run_versioned_migrations, DbPool,
        Migration, MigrationFailurePolicy, MigrationGovernanceConfig, PromotionConfig,
    };
    use anyhow::Result;
    use serial_test::serial;
    use sqlx::postgres::PgPoolOptions;

    fn require_db_tests() -> bool {
        std::env::var("KRAB_REQUIRE_DB_TESTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    }

    /// Connect to the test database (`DATABASE_URL`, defaulting to a local
    /// `krab_test`), or skip the calling test.
    ///
    /// The skip is deliberately LOUD and controllable: these tests used to
    /// early-return silently on connection failure, so a CI runner with no
    /// Postgres reported the whole migration-governance suite as passing
    /// without executing a single statement. Now:
    ///
    /// - `KRAB_REQUIRE_DB_TESTS=1` (CI mode): a connection failure PANICS —
    ///   an unreachable database fails the suite instead of greenwashing it.
    /// - otherwise: an unmistakable `SKIPPED` line goes to stderr before the
    ///   test returns early.
    async fn test_pool_or_skip(test_name: &str) -> Option<DbPool> {
        // `KRAB_TEST_DATABASE_URL` wins over `DATABASE_URL`: the secret-policy
        // tests in `config.rs` legitimately set, clear, and overwrite
        // `DATABASE_URL` while exercising sourcing rules, so in a full-suite
        // run the ambient `DATABASE_URL` is unreliable by design. A dedicated
        // variable nothing else touches keeps the live-database connection
        // stable; `DATABASE_URL` and the localhost default remain fallbacks
        // for single-test gate runs and CI compose files.
        let url = std::env::var("KRAB_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/krab_test".to_string());
        match PgPoolOptions::new().max_connections(1).connect(&url).await {
            Ok(pool) => Some(pool),
            Err(err) => {
                if require_db_tests() {
                    panic!(
                        "KRAB_REQUIRE_DB_TESTS is set but the test database at '{url}' is \
                         unreachable for {test_name}: {err}"
                    );
                }
                eprintln!(
                    "SKIPPED {test_name}: test database at '{url}' not available ({err}). \
                     This test executed NOTHING. Set KRAB_REQUIRE_DB_TESTS=1 to make an \
                     unreachable database a hard failure (CI mode)."
                );
                None
            }
        }
    }

    async fn clean_test_db(pool: &DbPool) -> Result<()> {
        sqlx::query("DROP TABLE IF EXISTS krab_migration_policy_audit, krab_migration_schema_ownership, krab_migration_rollback_rehearsals, krab_migrations, krab_migration_environment, user_audit_log, user_profiles, users, multi_stmt_items")
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
    #[serial]
    async fn test_migration_lifecycle() {
        let Some(pool) = test_pool_or_skip("test_migration_lifecycle").await else {
            return;
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
    #[serial]
    async fn test_migration_rollback() {
        let Some(pool) = test_pool_or_skip("test_migration_rollback").await else {
            return;
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
    #[serial]
    async fn test_drift_detection() {
        let Some(pool) = test_pool_or_skip("test_drift_detection").await else {
            return;
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
    #[serial]
    async fn test_governance_release_requires_rehearsal_artifact() {
        let Some(pool) =
            test_pool_or_skip("test_governance_release_requires_rehearsal_artifact").await
        else {
            return;
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
    #[serial]
    async fn test_governance_release_passes_with_rehearsal_artifact() {
        let Some(pool) =
            test_pool_or_skip("test_governance_release_passes_with_rehearsal_artifact").await
        else {
            return;
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
    #[serial]
    async fn test_legacy_checksum_rows_are_rewritten_not_flagged() {
        let Some(pool) =
            test_pool_or_skip("test_legacy_checksum_rows_are_rewritten_not_flagged").await
        else {
            return;
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
    #[serial]
    async fn test_true_checksum_mismatch_is_still_flagged() {
        let Some(pool) = test_pool_or_skip("test_true_checksum_mismatch_is_still_flagged").await
        else {
            return;
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

    #[tokio::test]
    #[serial]
    async fn test_multi_statement_migration_applies_and_rolls_back() {
        let Some(pool) =
            test_pool_or_skip("test_multi_statement_migration_applies_and_rolls_back").await
        else {
            return;
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        // A single migration whose body holds TWO statements. Under the
        // prepared-statement protocol (`sqlx::query`) this fails at runtime;
        // it must execute via the simple-query protocol (`sqlx::raw_sql`).
        let migrations = vec![Migration {
            version: 1,
            name: "create_items_with_index",
            sql: "CREATE TABLE multi_stmt_items (id BIGSERIAL PRIMARY KEY, label TEXT NOT NULL); \
                  CREATE INDEX idx_multi_stmt_items_label ON multi_stmt_items(label);",
            rollback_sql: Some(
                "DROP INDEX IF EXISTS idx_multi_stmt_items_label; \
                 DROP TABLE IF EXISTS multi_stmt_items;",
            ),
            critical: true,
            destructive: false,
        }];

        let report = run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)
            .await
            .expect("multi-statement migration must apply");
        assert_eq!(report.applied_versions, vec![1]);

        // Both statements executed: table AND index exist.
        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_tables WHERE tablename = 'multi_stmt_items')",
        )
        .fetch_one(&pool)
        .await
        .expect("failed to check table existence");
        assert!(table_exists, "first statement (CREATE TABLE) must execute");

        let index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE indexname = 'idx_multi_stmt_items_label')",
        )
        .fetch_one(&pool)
        .await
        .expect("failed to check index existence");
        assert!(index_exists, "second statement (CREATE INDEX) must execute");

        // Multi-statement rollback bodies must execute the same way.
        rollback_to_version(&pool, &migrations, 0)
            .await
            .expect("multi-statement rollback must execute");

        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_tables WHERE tablename = 'multi_stmt_items')",
        )
        .fetch_one(&pool)
        .await
        .expect("failed to re-check table existence");
        assert!(!table_exists, "rollback must drop the table");
    }

    #[tokio::test]
    #[serial]
    async fn test_governance_deny_records_audit_row_then_errors() {
        let Some(pool) =
            test_pool_or_skip("test_governance_deny_records_audit_row_then_errors").await
        else {
            return;
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        // Drive the deny through the real env knob, exactly as an operator
        // would set it. Safe here: the whole db suite is #[serial].
        std::env::set_var("DB_MIGRATION_ALLOW_APPLY", "false");
        let from_env = MigrationGovernanceConfig::from_env();
        std::env::remove_var("DB_MIGRATION_ALLOW_APPLY");
        assert!(!from_env.allow_apply);

        let cfg = MigrationGovernanceConfig {
            service_name: "service_users".to_string(),
            // "dev" is not a release environment, so the rehearsal gate stays
            // out of the picture and the deny is attributable to allow_apply.
            environment: "dev".to_string(),
            ..from_env
        };

        let err = enforce_migration_governance(&pool, &cfg)
            .await
            .expect_err("DB_MIGRATION_ALLOW_APPLY=false must deny with an error, not Ok(())");
        let message = format!("{err:#}");
        assert!(
            message.contains("DB_MIGRATION_ALLOW_APPLY"),
            "deny error should name the governing variable: {message}"
        );

        // Record-then-deny: the audit row must exist despite the error.
        let decision: String = sqlx::query_scalar(
            "SELECT decision FROM krab_migration_policy_audit
             WHERE service_name = $1 AND policy_name = 'migration_governance'
             ORDER BY id DESC LIMIT 1",
        )
        .bind("service_users")
        .fetch_one(&pool)
        .await
        .expect("audit row must be written before the deny error");
        assert_eq!(decision, "deny");
    }

    #[tokio::test]
    #[serial]
    async fn test_promotion_unknown_recorded_environment_errors() {
        let Some(pool) =
            test_pool_or_skip("test_promotion_unknown_recorded_environment_errors").await
        else {
            return;
        };
        clean_test_db(&pool).await.expect("failed to clean db");

        // Plant a recorded environment outside the ladder, as a misconfigured
        // deployment would ("production" instead of "prod"). This used to
        // resolve to index 0 = "local", making any target look like a forward
        // promotion.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS krab_migration_environment (environment TEXT PRIMARY KEY, updated_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        )
        .execute(&pool)
        .await
        .expect("failed to create environment table");
        sqlx::query("INSERT INTO krab_migration_environment (environment) VALUES ('production')")
            .execute(&pool)
            .await
            .expect("failed to plant unknown environment");

        let cfg = PromotionConfig {
            environment: "prod".to_string(),
            allow_apply: true,
        };
        let err = enforce_promotion_policy(&pool, &cfg)
            .await
            .expect_err("an unknown recorded environment must error, not pass as 'local'");
        let message = format!("{err:#}");
        assert!(
            message.contains("production"),
            "error should name the unknown environment: {message}"
        );
        assert!(
            message.contains("staging"),
            "error should list the known ladder: {message}"
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
