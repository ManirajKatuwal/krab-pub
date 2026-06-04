# Krab

Krab is a full-stack Rust framework for server-side rendering, island hydration, service composition, and production-oriented operational controls.

---

## Overview

Krab is a Rust-native full-stack framework designed to combine the ergonomics of modern web frameworks with Rust’s performance, memory safety, and type guarantees.

It uses a server-side rendering architecture with island hydration: pages are rendered as HTML on the server, while interactive components are selectively hydrated with WebAssembly on the client. This reduces client-side payload size while preserving interactivity where it is needed.

Krab also includes built-in service composition for frontend, authentication, and user-domain services, together with production-focused operational controls such as migration governance, dependency policy enforcement, structured telemetry, and secret-management support.

### Why Krab over alternatives?

| Concern                     | Axum / Actix | Leptos / Dioxus | Next.js      | **Krab**                                                     |
| --------------------------- | ------------ | --------------- | ------------ | ------------------------------------------------------------ |
| SSR + Islands               | Manual       | Full WASM SPA   | JS-based ISR | Native Rust SSR with selective WASM hydration                |
| Multi-service orchestration | DIY          | Not included    | Not included | Built-in orchestrator and service composition                |
| Migration governance        | DIY          | DIY             | DIY          | Checksum validation, drift detection, and rollback rehearsal |
| Dependency security         | DIY          | DIY             | npm audit    | `cargo-deny` CI gates for advisories, licenses, and bans     |
| Secret management           | DIY          | DIY             | DIY          | `*_FILE` and vault-reference sourcing in production          |
| Telemetry and SLOs          | DIY          | DIY             | DIY          | Prometheus metrics, RED/USE taxonomy, and burn-rate alerts   |

---

## Quick Start

### Prerequisites

- **Rust** stable 1.75+ ([rustup.rs](https://rustup.rs/))
- **PostgreSQL** 15+ (production backend) or **SQLite** (lightweight/dev)
- **wasm-pack** (for WASM client builds): `cargo install wasm-pack`
- **cargo-deny** (for dependency auditing): `cargo install cargo-deny`

### Setup

```sh
# Clone and configure
git clone https://github.com/your-org/krab.git
cd krab
cp .env.example .env
# Edit .env with your local settings (DATABASE_URL, KRAB_AUTH_MODE, etc.)
```

### Run services

```sh
# Individual services
cargo run --bin service_auth       # Auth REST API         → localhost:3001
cargo run --bin service_users      # Users GraphQL API     → localhost:3002
cargo run --bin service_frontend   # SSR Frontend          → localhost:3000

# Or use the orchestrator to run all at once
cargo run --bin krab_orchestrator
```

### Validate the workspace

```sh
cargo fmt --all --check                                    # Formatting
cargo clippy --workspace --all-targets -- -D warnings      # Linting
cargo test --workspace                                     # Tests
cargo deny --all-features check advisories licenses bans   # Dependency audit
cargo doc --workspace --no-deps                            # Generate rustdoc
```

---

## Architecture

Krab follows a server-first, client-opt-in architecture organized as a Cargo workspace.

```
┌──────────────────────────────────────────────────────────┐
│                    krab_orchestrator                      │
│              (multi-process service runner)               │
├────────────┬─────────────────┬────────────────────────────┤
│service_auth│ service_users   │      service_frontend      │
│  (REST)    │ (GraphQL + DB)  │     (SSR + Islands)        │
├────────────┴─────────────────┴────────────────────────────┤
│                        krab_core                          │
│  config · http · db · telemetry · resilience · signal     │
├──────────────────┬────────────────────────────────────────┤
│   krab_macros    │  krab_server  │  krab_client (WASM)    │
│ (view!, #[island])│ (Hyper/Tower) │ (Island hydration)    │
├──────────────────┴────────────────────────────────────────┤
│                      krab_cli                             │
│            (dev tooling, env-check, bootstrap)            │
└──────────────────────────────────────────────────────────┘
```

### Workspace crates

| Crate                                                    | Purpose                                                                                 |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| [`krab_core`](crates/framework/krab_core/)               | Shared config, HTTP middleware, resilience, telemetry, DB governance, and signal system |
| [`krab_macros`](crates/framework/krab_macros/)           | Procedural macros (`view!`, `#[island]`)                                                |
| [`krab_client`](crates/framework/krab_client/)           | WASM runtime for island hydration (browser)                                             |
| [`krab_server`](crates/framework/krab_server/)           | Hyper/Tower server foundations                                                          |
| [`service_auth`](services/service_auth/)                 | Authentication service (REST — JWT/OIDC token issuance)                                 |
| [`service_users`](services/service_users/)               | Users service (GraphQL + PostgreSQL/SQLite)                                             |
| [`service_frontend`](services/service_frontend/)         | SSR frontend service with island hydration                                              |
| [`krab_orchestrator`](crates/tooling/krab_orchestrator/) | Multi-service process orchestrator                                                      |
| [`krab_cli`](crates/tooling/krab_cli/)                   | Developer CLI helpers (env-check, bootstrap)                                            |

---

## Features

### Islands architecture

Pages are server-rendered as static HTML by default. Interactive components are marked with `#[island]` and selectively hydrated via WebAssembly:

```rust
#[island]
pub fn Counter(initial: i32) -> impl View {
    let (count, set_count) = create_signal(initial);
    view! {
        <button on:click=move |_| set_count.update(|n| *n += 1)>
            "Count: " {count}
        </button>
    }
}
```

- Server: renders HTML
- Client: downloads targeted WASM and attaches event listeners to the existing DOM

### Multi-database support

Krab supports pluggable database backends via the `KRAB_DB_DRIVER` environment variable:

| Driver         | Value                | Use Case                                        | Vulnerability Status |
| -------------- | -------------------- | ----------------------------------------------- | -------------------- |
| **PostgreSQL** | `postgres` (default) | Production-grade with full migration governance | Clean                |
| **SQLite**     | `sqlite`             | Lightweight dev/testing, portable deployments   | Clean                |

PostgreSQL includes enterprise features: versioned migrations with checksums, drift detection, promotion policy enforcement, and rollback rehearsal requirements.

### Authentication and security

- **JWT/OIDC** token issuance with key rotation (`KeyRing` with multiple `kid` support)
- **Rate limiting** with configurable capacity/refill and explicit store-failure policy (`KRAB_RATE_LIMIT_FAIL_OPEN`)
- **JWT algorithm allowlist** via `KRAB_JWT_ALLOWED_ALGS`
- **Proxy trust controls** via `KRAB_TRUST_PROXY_HEADERS` (forwarded headers are untrusted by default)
- **Opt-in CSRF middleware** for cookie-based unsafe requests (`KRAB_CSRF_ENABLED` / `KRAB_AUTH_COOKIE_SESSION_ENABLED`)
- **Production secret enforcement**: Inline secrets are rejected in non-dev environments; must use `*_FILE` or `*_VAULT_REF` sourcing
- **RBAC**: Admin scope/role gating on protected endpoints
- **Token lifecycle**: Issue, refresh (with replay detection), revoke

### Dependency security

Zero-tolerance dependency governance enforced via `cargo-deny`:

- **Advisories**: All known vulnerabilities denied (no `ignore` entries)
- **Licenses**: Allowlist-only (MIT, Apache-2.0, CC0-1.0, BSD, ISC, Zlib, MPL-2.0, Unicode)
- **Bans**: Unknown registries and git sources denied
- **CI gate**: `cargo deny --all-features check advisories licenses bans`

### Observability

- **Structured logging** via `tracing` with OpenTelemetry-aligned field names
- **Prometheus metrics** at `/metrics/prometheus` on every service
- **Prometheus compatibility aliases** maintained for legacy dashboard/alert migration (`krab_response_5xx_total`, `krab_request_duration_ms_*`)
- **JSON metrics** at `/metrics` for programmatic consumption
- **Health** (`/health`) and **readiness** (`/ready`) endpoints with dependency status
- **Request correlation** via `x-request-id` header propagation

---

## Configuration

All configuration is via environment variables. See [`.env.example`](.env.example) for the complete reference.

### Core variables

| Variable           | Description                                    | Default             |
| ------------------ | ---------------------------------------------- | ------------------- |
| `KRAB_ENVIRONMENT` | Runtime environment (`dev`, `staging`, `prod`) | `dev`               |
| `KRAB_AUTH_MODE`   | Authentication mode (`jwt`, `oidc`, `static`)  | `jwt`               |
| `KRAB_DB_DRIVER`   | Database backend (`postgres`, `sqlite`)        | `postgres`          |
| `DATABASE_URL`     | Database connection string                     | Per-driver default  |
| `KRAB_HOST`        | Service bind host                              | `127.0.0.1`         |
| `KRAB_PORT`        | Service bind port                              | Per-service default |
| `RUST_LOG`         | Log filter directive                           | `info`              |

### Secret sourcing (production)

In `staging` and `prod` environments, secrets must be provided via file mount or vault reference:

| Secret             | File Variable                       | Vault Variable                           |
| ------------------ | ----------------------------------- | ---------------------------------------- |
| JWT signing key    | `KRAB_JWT_SECRET_FILE`              | `KRAB_JWT_SECRET_VAULT_REF`              |
| JWT key ring       | `KRAB_JWT_KEYS_JSON_FILE`           | `KRAB_JWT_KEYS_JSON_VAULT_REF`           |
| Bootstrap password | `KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE` | `KRAB_AUTH_BOOTSTRAP_PASSWORD_VAULT_REF` |
| Login users        | `KRAB_AUTH_LOGIN_USERS_JSON_FILE`   | `KRAB_AUTH_LOGIN_USERS_JSON_VAULT_REF`   |
| Database URL       | `DATABASE_URL_FILE`                 | —                                        |

For the full environment template with validation rules, see [`plans/environment_template.md`](plans/environment_template.md).

---

## Database

### PostgreSQL (default, production-grade)

Full enterprise governance is available when using PostgreSQL:

- **Versioned migrations** with checksum integrity validation
- **Drift detection** comparing expected vs. applied migration state
- **Promotion policy**: Enforces `local → dev → staging → prod` ordering
- **Rollback rehearsal**: Required before release environment promotions
- **Governance audit trail**: All policy decisions recorded to `krab_migration_policy_audit`
- **Security validation**: Rejects default credentials in non-dev environments

### SQLite (lightweight alternative)

SQLite is available for development, testing, and lightweight deployments:

```sh
KRAB_DB_DRIVER=sqlite DATABASE_URL="sqlite://krab_users.sqlite?mode=rwc" cargo run --bin service_users
```

---

## CI/CD Gates

All gates must pass before merge. Automated workflows enforce quality at every PR:

| Workflow            | File                                                                                       | Purpose                                          |
| ------------------- | ------------------------------------------------------------------------------------------ | ------------------------------------------------ |
| Ops Hardening       | [`.github/workflows/ops-hardening.yaml`](.github/workflows/ops-hardening.yaml)             | `fmt`, `clippy`, `rustdoc`, and `cargo-deny`     |
| Dependency Security | [`.github/workflows/dependency-security.yaml`](.github/workflows/dependency-security.yaml) | `cargo-audit` and SBOM generation                |
| API Contract        | [`.github/workflows/api-contract.yaml`](.github/workflows/api-contract.yaml)               | API contract validation                          |
| DB Lifecycle        | [`.github/workflows/db-lifecycle.yaml`](.github/workflows/db-lifecycle.yaml)               | Migration, rollback simulation, and drift checks |
| E2E Depth           | [`.github/workflows/e2e-depth.yaml`](.github/workflows/e2e-depth.yaml)                     | Multi-service end-to-end testing                 |
| NFT Suite           | [`.github/workflows/nft.yaml`](.github/workflows/nft.yaml)                                 | Non-functional and load-testing gates            |

---

## Documentation Map

### Root documents

| Document                                 | Purpose                                                         |
| ---------------------------------------- | --------------------------------------------------------------- |
| [`README.md`](README.md)                 | This document — project overview and quick start                |
| [`CONTRIBUTING.md`](CONTRIBUTING.md)     | Contribution workflow, quality gates, and engineering standards |
| [`CHANGELOG.md`](CHANGELOG.md)           | Release history (Keep a Changelog format)                       |
| [`RELEASE_POLICY.md`](RELEASE_POLICY.md) | Release channels, promotion criteria, and versioning            |

### Technical references (`docs/`)

| Document                                                     | Purpose                                                         |
| ------------------------------------------------------------ | --------------------------------------------------------------- |
| [`docs/API.md`](docs/API.md)                                 | Public API contract for all HTTP and GraphQL endpoints          |
| [`docs/signal_safety.md`](docs/signal_safety.md)             | Signal system threading constraints and SSR usage patterns      |
| [`docs/security.md`](docs/security.md)                       | Security architecture, secret management, and threat model      |
| [`docs/database.md`](docs/database.md)                       | Database architecture, migrations, and multi-driver support     |
| [`docs/deployment.md`](docs/deployment.md)                   | Deployment guide for containerized and self-hosted environments |
| [`docs/reference_apps.md`](docs/reference_apps.md)           | Official reference app tracks and starter mapping               |
| [`docs/migration_guide.md`](docs/migration_guide.md)         | Migration notes from Axum, Leptos, and JS full-stack frameworks |
| [`docs/why_krab.md`](docs/why_krab.md)                       | Krab's product position and differentiators                     |
| [`docs/server_functions.md`](docs/server_functions.md)       | Server-function endpoint contract and safety patterns           |
| [`docs/service_composition.md`](docs/service_composition.md) | Service graph, topology, orchestrator, and boundary rules       |

### Planning documents (`plans/`)

| Document                                                                 | Purpose                                                           |
| ------------------------------------------------------------------------ | ----------------------------------------------------------------- |
| [`plans/01_vision_and_philosophy.md`](plans/01_vision_and_philosophy.md) | Core mission, pillars, and differentiators                        |
| [`plans/02_architecture_design.md`](plans/02_architecture_design.md)     | Detailed architecture: subsystems, routing, islands, data loading |
| [`plans/03_roadmap.md`](plans/03_roadmap.md)                             | Phase 0 roadmap, governance, epic breakdown, risk log             |
| [`plans/08_production_readiness.md`](plans/08_production_readiness.md)   | Production readiness checklist and gate definitions               |
| [`plans/oncall_playbook.md`](plans/oncall_playbook.md)                   | On-call runbook and incident response procedures                  |
| [`plans/db_rollback_runbook.md`](plans/db_rollback_runbook.md)           | Database rollback procedures and disaster recovery                |
| [`plans/environment_template.md`](plans/environment_template.md)         | Environment variable reference and validation                     |

---

## Security

Report vulnerabilities privately via [GitHub Security Advisories](../../security/advisories).

Current security posture:

- Zero `cargo-deny` advisory ignores
- Production secret sourcing enforced via `*_FILE` and `*_VAULT_REF`
- Inline secrets rejected in non-development environments
- No panic-driven startup paths
- Rate limiting on authentication endpoints
- Configurable rate-limit store failure policy via `KRAB_RATE_LIMIT_FAIL_OPEN`
- JWT algorithm allowlist enforcement via `KRAB_JWT_ALLOWED_ALGS`
- Proxy headers untrusted by default via `KRAB_TRUST_PROXY_HEADERS=false`
- CORS, compression, and request-id middleware on all services
- Non-development startup rejects empty CORS allowlists, requiring `KRAB_CORS_ORIGINS` in staging and production

For the full security architecture, see [`docs/security.md`](docs/security.md).

---

## Contributors

| Role                | GitHub                                               |
| ------------------- | ---------------------------------------------------- |
| Primary Contributor | [@manirajkatuwal](https://github.com/manirajkatuwal) |

---

## License

MIT — see individual crate `Cargo.toml` files for per-crate license declarations.
