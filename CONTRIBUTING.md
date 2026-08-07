# Contributing to Krab

Thank you for your interest in contributing to Krab! This guide defines the required workflow, quality gates, and engineering standards for all contributions.

---

## Prerequisites

1. Install stable Rust (1.75+) via [rustup](https://rustup.rs/).
2. Install required tools:
   ```sh
   cargo install wasm-pack        # WASM client builds
   cargo install cargo-deny       # Dependency auditing
   ```
3. Copy [`.env.example`](.env.example) to `.env` and configure local values.
4. Start PostgreSQL 15+ or set `KRAB_DB_DRIVER=sqlite` for local development.

---

## Local Verification (required before PR)

Run all of the following from the repository root:

```sh
# Code quality
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

# Tests
cargo test --workspace

# Dependency governance
cargo run --package krab_cli -- security dependency-gate --diagnostics

# Documentation
cargo doc --workspace --no-deps
```

If your change affects WASM/frontend behavior:

```sh
wasm-pack build krab_client --release --target web
```

---

## Branch and Pull Request Policy

- Branch from `main`.
- Use descriptive prefixes: `feat/`, `fix/`, `chore/`, `docs/`, `refactor/`, `security/`.
- Keep a PR focused on one logical objective.
- Keep commit history clean; merge strategy is **squash merge**.
- Every required CI workflow must be green before merge.
- Breaking changes require migration notes in [`docs/reference/api.md`](docs/reference/api.md) and [`CHANGELOG.md`](CHANGELOG.md).

---

## Engineering Standards

### Rust Code

- **No panics in startup paths**: All service boot sequences must return typed `Result` errors via `anyhow`. Never use `unwrap()`, `expect()`, or `panic!()` in critical paths.
- **Structured logging**: Use `tracing` crate with stable, snake_case event names:
  ```rust
  tracing::info!(
      service = %service_name,
      environment = %environment,
      "event_name_in_snake_case"
  );
  ```
- **OpenTelemetry keys**: Use OTel-aligned field names where applicable (`http.method`, `http.status_code`, `http.route`, `duration_ms`).
- **Configuration**: New configuration knobs must be documented in [`.env.example`](.env.example) and [`docs/reference/environment.md`](docs/reference/environment.md).
- **Secrets**: Use `krab_core::config::read_env_or_file()` for any sensitive configuration. Never hardcode secrets.

### Database Migrations

- Add migrations to the owning service's migration list with a unique version number.
- Every migration must provide `rollback_sql` unless explicitly irreversible and documented.
- Destructive migrations (`destructive: true`) **must** include `rollback_sql`.
- Promotion to staging/production requires rollback rehearsal evidence.
- See [`docs/reference/database.md`](docs/reference/database.md) for the full migration governance model.

### Dependencies

- New dependencies must be compatible with the license allowlist in [`deny.toml`](deny.toml).
- Verify with `cargo deny check licenses` before submitting.
- Avoid dependencies with known RUSTSEC advisories — the CI gate will reject them.

---

## CI Gates

| Gate | Workflow | Required |
|---|---|---|
| Formatting / Linting / Tests | `ops-hardening` | Yes |
| Dependency governance | `ops-hardening` (`cargo-deny`) | Yes |
| API contract checks | `api-contract` | Yes |
| DB lifecycle checks | `db-lifecycle` | Yes |
| E2E depth checks | `e2e-depth` | Yes |
| NFT suite | `nft` | When triggered by workflow/label policy |

---

## Security Reporting

Report vulnerabilities privately via [GitHub Security Advisories](../../security/advisories).

- For the full security architecture, see [`docs/reference/security.md`](docs/reference/security.md).

---

## Documentation

When contributing documentation:

- Update relevant docs in `docs/` for technical reference changes.
- Update [`CHANGELOG.md`](CHANGELOG.md) for notable changes.
- Use relative links between documentation files.
- Keep markdown formatting consistent with existing docs.

---

## Getting Help

- Review the [Architecture section in README.md](README.md#architecture) for an overview.
- Check [`docs/reference/api.md`](docs/reference/api.md) for endpoint contracts.
- Browse [`docs/`](docs/README.md) for the full documentation index.

---

## Primary Contributor

[@manirajkatuwal](https://github.com/manirajkatuwal)
