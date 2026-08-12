# Database Architecture

This document covers Krab's database layer, multi-driver support, migration governance, and operational procedures.

---

## Overview

Krab's database layer is centralized in `krab_core::db` and provides:

- Pluggable database backends via `KRAB_DB_DRIVER`
- Connection pooling with retry logic and exponential backoff
- Versioned migration engine with checksum integrity
- Drift detection and promotion policy enforcement
- Rollback rehearsal governance for release environments
- Security validation of database credentials

---

## Supported Drivers

| Driver | `KRAB_DB_DRIVER` | Cargo feature | Default URL | Governance |
|---|---|---|---|---|
| **PostgreSQL** | `postgres` | `db-postgres` | `postgres://postgres@localhost:5432/krab_users` | Full — migrations, drift, promotion, rollback |
| **SQLite** | `sqlite` | `db-sqlite` | `sqlite://krab.sqlite?mode=rwc` | None — driver only |

Compiling a driver in and selecting one at runtime are separate steps. Enable
whichever features you need, then pick between them with `KRAB_DB_DRIVER`:

```toml
# Postgres only — the usual production choice
krab_core = { version = "0.4.0", features = ["db-postgres"] }

# Both, selected per environment
krab_core = { version = "0.4.0", features = ["db-postgres", "db-sqlite"] }
```

> **`db` is a deprecated alias for `db-postgres`.** It is kept for one minor
> version per the breaking-change policy in
> [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md). It was deprecated *in* `0.2.0`,
> so it is removable no earlier than `0.3.0` — matching the manifest comment in
> `krab_core/Cargo.toml`. Use the explicit driver feature.
>
> Before this split, `krab_core`'s `sqlx` dependency enabled `postgres`
> unconditionally and nothing else. SQLite existed only inside
> `services/service_users`, a reference application, so a framework consumer
> could not select it however the documentation described `KRAB_DB_DRIVER`.

### Selecting a driver

```sh
# PostgreSQL (default — recommended for production)
KRAB_DB_DRIVER=postgres DATABASE_URL="postgres://user:pass@host:5432/dbname"

# SQLite (development / testing / portable)
KRAB_DB_DRIVER=sqlite DATABASE_URL="sqlite://krab.sqlite?mode=rwc"
```

`krab_core::db::resolve_db_driver()` reads the variable and returns a
[`DbDriver`]. **An unset or blank value selects Postgres**, not SQLite — a
service that quietly fell back to a local SQLite file because a variable was
missing would lose migration governance without failing. Ask
`DbDriver::supports_migration_governance()` rather than matching on the variant,
so a driver added later does not silently inherit guarantees it cannot honour.

[`DbDriver`]: https://docs.rs/krab_core/latest/krab_core/db/enum.DbDriver.html

---

## Connection Configuration

All pool settings are configurable via environment variables:

| Variable | Description | Default |
|---|---|---|
| `DATABASE_URL` | Connection string (also supports `DATABASE_URL_FILE`) | Driver-specific default |
| `DB_MAX_CONNECTIONS` | Maximum pool connections | `10` |
| `DB_MIN_CONNECTIONS` | Minimum idle connections | `1` |
| `DB_ACQUIRE_TIMEOUT_SECS` | Connection acquire timeout | `5` |
| `DB_MAX_LIFETIME_SECS` | Maximum connection lifetime | `1800` (30 min) |
| `DB_IDLE_TIMEOUT_SECS` | Idle connection timeout | `600` (10 min) |
| `DB_CONNECT_RETRIES` | Number of connection retry attempts | `5` |
| `DB_CONNECT_RETRY_DELAY_MS` | Base delay between retries (exponential backoff applied) | `750` |

### Connection retry behavior

Failed connections use **exponential backoff with deterministic jitter**:

- Base delay is multiplied by `2^(attempt-1)`, capped at 30 seconds.
- A deterministic jitter (0–250ms, based on URL + attempt hash) is added to reduce thundering herd effects across replicas.

---

## Migration Engine (PostgreSQL)

### Versioned migrations

Each migration is defined as a `Migration` struct:

```rust
Migration {
    version: 1,
    name: "create_users",
    sql: "CREATE TABLE IF NOT EXISTS users (...)",
    rollback_sql: Some("DROP TABLE IF EXISTS users"),
    critical: true,
    destructive: false,
}
```

Key guarantees:

- **Checksum integrity**: Every migration's SQL is checksummed (SHA-256, hex) at apply time. Re-applying a migration with modified SQL fails with a checksum mismatch error. Rows written by versions before the SHA-256 switch (which used std's `DefaultHasher`) are recognised and rewritten in place on the next migration run or drift check, provided that upgrade pass happens **before** a Rust toolchain bump — `DefaultHasher` output is not stable across releases, which is why the format changed.
- **Idempotent**: Already-applied migrations are skipped.
- **Destructive guard**: Migrations marked `destructive: true` must provide `rollback_sql` or the engine refuses to proceed.
- **Failure policy**: Configurable via `DB_MIGRATION_FAILURE_POLICY` (`halt` or `continue_non_critical`).

### Drift detection

After migrations run, the engine compares expected vs. applied state:

```rust
let drift = detect_migration_drift(&pool, &migrations).await?;
// drift.missing_versions    — expected but not applied
// drift.unexpected_versions — applied but not expected
// drift.checksum_mismatches — applied with different checksum
```

### Promotion policy

Enforces deployment ordering: `local → dev → staging → prod`.

- Backward promotion (e.g., `prod → dev`) is rejected.
- Skipping stages (e.g., `dev → prod`) triggers a warning.
- Policy decisions are audited in the `krab_migration_environment` table.

Configuration:

| Variable | Description | Default |
|---|---|---|
| `DB_MIGRATION_ALLOW_APPLY` | Whether to allow automatic migration application | `true` |
| `DB_MIGRATION_FAILURE_POLICY` | Failure handling (`halt`, `continue_non_critical`) | `halt` |
| `DB_MIGRATION_RELEASE_ENVIRONMENTS` | Comma-separated release environments | `staging,prod` |
| `DB_MIGRATION_REQUIRE_REHEARSAL_IN_RELEASE` | Require rollback rehearsal in release environments | `true` |

### Rollback rehearsal governance

In release environments (`staging`, `prod`), the migration engine requires evidence of a successful rollback rehearsal:

1. Rollback rehearsals are recorded in `krab_migration_rollback_rehearsals`.
2. Before applying migrations in a release environment, the engine checks for a passing rehearsal for the current service.
3. If no rehearsal exists, migration is blocked with a governance violation error.

---

## Schema (PostgreSQL — `service_users`)

The users service maintains these tables via versioned migrations:

| Version | Migration | Critical |
|---|---|---|
| 1 | `create_users` — Core users table | Yes |
| 2 | `create_users_created_at_index` | No |
| 3 | `create_user_profiles` — User profiles (display name, bio, avatar) | No |
| 4 | `create_user_audit_log` — Audit trail for user actions | No |
| 5 | `create_user_audit_log_created_at_index` | No |
| 6 | `add_tenant_id_to_users` — Multi-tenancy support | Yes |
| 7 | `create_users_tenant_id_index` | No |

### Governance tables (auto-created by `krab_core`)

| Table | Purpose |
|---|---|
| `krab_migrations` | Migration version/checksum registry |
| `krab_migration_environment` | Promotion policy state tracking |
| `krab_migration_schema_ownership` | Service-to-schema ownership mapping |
| `krab_migration_policy_audit` | Governance decision audit trail |
| `krab_migration_rollback_rehearsals` | Rollback rehearsal evidence |

---

## Schema (SQLite — `service_users`)

SQLite uses a simplified bootstrap (no governance tables):

```sql
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    tenant_id TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_users_created_at ON users(created_at);
CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON users(tenant_id);
```

---

## Security Validation

In non-dev environments, `DbConfig::validate_security()` enforces:

- `DATABASE_URL` must be set (non-empty).
- Default credentials (e.g., `postgres:password@`) are rejected.
- Superuser usage (`postgres` role) in production triggers a warning.

---

## Rollback Procedures

For detailed rollback procedures, see [`docs/operations/db_rollback_runbook.md`](../operations/db_rollback_runbook.md).

Quick reference:

```rust
// Roll back to a specific version
rollback_to_version(&pool, &migrations, target_version).await?;
```

The rollback engine:
1. Sorts migrations in reverse version order.
2. For each migration above the target version that has been applied, executes the `rollback_sql` in a transaction.
3. Removes the migration record from `krab_migrations`.
