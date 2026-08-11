# Database Rollback Runbook

This document outlines the procedures for rolling back database migrations in the Krab framework.

## 1. Overview

The rollback system relies on the `rollback_to_version` function in `krab_core/src/db.rs` and the `_krab_migrations` tracking table. All destructive migrations MUST have a corresponding `rollback_sql` definition.

### Governance Scope

Migration governance is enforced by startup checks through `enforce_migration_governance` and promotion checks through `enforce_promotion_policy`.

Governance records are persisted in:

- `krab_migration_schema_ownership` (service-level schema ownership by environment)
- `krab_migration_rollback_rehearsals` (rollback rehearsal evidence artifacts)
- `krab_migration_policy_audit` (allow/deny decisions and policy detail)

Required runtime configuration for release environments:

- `KRAB_SERVICE_NAME`
- `KRAB_ENVIRONMENT`
- `DB_MIGRATION_RELEASE_ENVIRONMENTS` (default: `staging,prod`)
- `DB_MIGRATION_REQUIRE_REHEARSAL_IN_RELEASE` (default: `true`)
- `DB_MIGRATION_ALLOW_APPLY`

## 2. Pre-Rollback Consistency Probes

Before initiating a rollback, verify the system state:

1. **Verify current version:**
   ```sql
   SELECT version, name, applied_at FROM krab_migrations ORDER BY version DESC LIMIT 1;
   ```

2. **Check for inflight transactions:**
   ```sql
   SELECT pid, state, query_start, query FROM pg_stat_activity WHERE state != 'idle';
   ```
   *Wait for long-running queries to complete or terminate them if necessary.*

3. **Check for dependent objects:**
   *Ensure no application code is relying on schema elements that will be dropped.*

4. **Verify locks:**
   ```sql
   SELECT relation::regclass, mode, granted FROM pg_locks WHERE NOT granted;
   ```
   *Ensure no deadlocks or blocking locks on target tables.*

5. **Verify schema ownership registration:**
   ```sql
   SELECT service_name, environment, updated_at
   FROM krab_migration_schema_ownership
   WHERE service_name = '<SERVICE_NAME>';
   ```

6. **Verify latest policy decision artifact:**
   ```sql
   SELECT policy_name, decision, detail, recorded_at
   FROM krab_migration_policy_audit
   WHERE service_name = '<SERVICE_NAME>'
   ORDER BY recorded_at DESC
   LIMIT 5;
   ```

## 3. Execution

### Automatic Rollback (via CLI/Tooling)

Execute the rollback command (to be implemented in CLI):
```bash
krab-cli db rollback --target-version <VERSION>
```

### Rollback Rehearsal Artifacting (Required for Release Environments)

Before production-bound promotion, perform rollback rehearsal in a staging-like environment and record evidence:

1. Execute rehearsal rollback and re-apply sequence.
2. Persist artifact (logs, SQL transcript, health checks, migration diff) in immutable storage.
3. Record the artifact in `krab_migration_rollback_rehearsals`:

   ```sql
   INSERT INTO krab_migration_rollback_rehearsals
   (service_name, environment, rollback_target, artifact_uri, succeeded)
   VALUES ('<SERVICE_NAME>', '<ENV>', <TARGET_VERSION>, '<ARTIFACT_URI>', true);
   ```

Without a successful rehearsal artifact, release-environment startup governance may block migration application.

### Manual Rollback (Emergency)

If the automated tooling is unavailable:

1. Connect to the database.
2. Identify the migration to rollback (highest version > target).
3. Execute the `rollback_sql` for that migration.
4. Delete the migration record:
   ```sql
   DELETE FROM krab_migrations WHERE version = <VERSION>;
   ```
5. Repeat for subsequent versions until target is reached.

## 4. Post-Rollback Verification

1. **Verify schema version:**
   ```sql
   SELECT max(version) FROM krab_migrations;
   ```
   *Should match target version.*

2. **Verify application health:**
   *Check `/health` and `/ready` endpoints of dependent services.*

3. **Check logs:**
   *Monitor service logs for schema mismatch errors.*

4. **Record verification evidence:**
   - migration version snapshot
   - readiness/health probe outputs
   - impacted service smoke test output
   - incident/change reference ID

5. **Confirm governance audit entry created:**
   ```sql
   SELECT service_name, environment, policy_name, decision, recorded_at
   FROM krab_migration_policy_audit
   WHERE service_name = '<SERVICE_NAME>'
   ORDER BY recorded_at DESC
   LIMIT 1;
   ```

## 5. Failure Recovery

If a rollback fails midway:

1. **Do not force further rollbacks.**
2. Inspect the error log.
3. Manually fix the schema state to match either the pre-rollback or post-rollback state.
4. Ensure the `krab_migrations` table accurately reflects the actual schema state.
