# CLAUDE.md

Guidance for Claude Code (and any AI agent) working in this repository.

---

## What this project is

**Krab** is a full-stack Rust web framework: server-side rendering with island
hydration (WASM), built-in multi-service composition, and production-oriented
operational governance (migration policy, dependency gates, telemetry, secret
sourcing).

It is a **Cargo workspace**, not an application. Changes here affect downstream
framework consumers, so API surface and governance artifacts matter as much as
the code.

- Version: `0.1.x` (workspace-wide, `[workspace.package]` in `Cargo.toml`)
- Edition: 2021, Rust stable 1.75+
- License: MIT

---

## Repository layout

```
crates/framework/
  krab_core/        Shared runtime: config, http*, db, telemetry, resilience,
                    signal, protocol, render_policy, isr, i18n, ws, server_fn
  krab_macros/      Proc macros: view!, #[island], #[server]  (+ trybuild tests)
  krab_client/      WASM island hydration runtime (browser)
  krab_server/      Hyper/Tower server foundations
crates/tooling/
  krab_cli/         `krab` CLI: dev workflow, generators, governance, release ops
  krab_orchestrator/ Multi-process service runner driven by krab.toml
services/
  service_auth/     REST auth (JWT/OIDC)              → :3001
  service_users/    GraphQL + Postgres/SQLite         → :3002
  service_frontend/ SSR + islands                     → :3000
  service_users_split/ Split-topology reference svc   → :3207
```

Supporting directories:

| Path | Contents | Tracked |
|---|---|---|
| [docs/](docs/) | All public documentation — see [docs/README.md](docs/README.md) | Yes |
| [benchmarks/](benchmarks/) | NFT thresholds, benchmark config, trend history | Inputs yes, results no |
| [scripts/](scripts/) | Python NFT/benchmark/evidence tooling, `check_health.ps1` | Yes |
| [monitoring/](monitoring/) | Prometheus config, alert rules, Grafana dashboard | Yes |
| [docker/](docker/) | Postgres init scripts, rendered NFT compose | Yes |
| [examples/](examples/) | Reference application tracks | Yes |
| [.github/workflows/](.github/workflows/) | 12 CI gate workflows | Yes |
| `internal/` | Plans, audits, evidence, wiki, reports | **No — gitignored** |

### The `internal/` boundary

Everything not for public distribution lives under `internal/`, covered by a
single [.gitignore](.gitignore) rule:

```
internal/plans/     planning documents, roadmaps, phase breakdowns
internal/audit/     audit reports, evidence bundles, generated CI evidence
internal/wiki/      engineering wiki
internal/reports/   strategy, framework, and pre-release reports
```

Links from tracked files into `internal/` resolve locally but not in a public
clone. **Never make `internal/` the only home for something a user needs** —
public behaviour belongs in `docs/`.

---

## Documentation map

Public documentation is organised by purpose:

| Directory | Holds | Examples |
|---|---|---|
| [docs/guides/](docs/guides/) | Task-oriented walkthroughs | `ide_setup.md`, `migration_guide.md`, `dev_workflow.md`, `reference_apps.md`, `why_krab.md` |
| [docs/reference/](docs/reference/) | Lookup material | `api.md`, `environment.md`, `database.md`, `security.md`, `deployment.md`, `server_functions.md` |
| [docs/architecture/](docs/architecture/) | How and why it is built | `vision.md`, `design.md`, `hydration.md`, `render_policy.md`, `service_composition.md`, `signal_safety.md`, `protocol_flexibility.md` |
| [docs/operations/](docs/operations/) | Running it in production | `oncall_playbook.md`, `db_rollback_runbook.md`, `slo_alerts.md`, `api_governance.md`, `production_readiness.md` |
| [docs/adr/](docs/adr/) | Architecture Decision Records | numbered, immutable once accepted |

`docs/guides/dev_workflow.md` is **generated** by `krab docs` — edit the
generator in `crates/tooling/krab_cli/src/dev_workflow.rs`, not the output.

---

## Commands

Shell here is **PowerShell on Windows**. Prefer `;` sequencing over `&&`.
`cargo` commands are identical across platforms.

### Verification (run before declaring any change done)

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
cargo run --package krab_cli -- security dependency-gate --diagnostics
```

### Running services

```sh
cargo run --bin service_auth        # :3001
cargo run --bin service_users       # :3002
cargo run --bin service_frontend    # :3000
cargo run --bin krab_orchestrator   # all of the above, per krab.toml
```

### CLI governance commands (same binaries CI runs)

```sh
cargo run -p krab_cli -- doctor --diagnostics --strict
cargo run -p krab_cli -- env-check --strict
cargo run -p krab_cli -- contract check --diagnostics
cargo run -p krab_cli -- contract protocol-check --diagnostics
cargo run -p krab_cli -- db lifecycle --diagnostics
cargo run -p krab_cli -- db rollback --diagnostics
cargo run -p krab_cli -- db drift --diagnostics
cargo run -p krab_cli -- db rehearsal
cargo run -p krab_cli -- topology doctor --diagnostics
cargo run -p krab_cli -- release check --diagnostics --json
cargo run -p krab_cli -- release certify --out internal/audit/release-certify/<id> --json
```

Generated output defaults, all under gitignored paths:

| Command | Writes to |
|---|---|
| `krab db rehearsal` | `internal/audit/evidence/rollback-rehearsal-evidence.txt` |
| `krab release certify` | `internal/audit/release-certify/` |
| `krab bootstrap` (orchestrator logs) | `internal/audit/orchestrator/` |
| `krab docs` | `docs/guides/dev_workflow.md` (tracked) |

### Feature-gated test targets

`krab_core` has **no default features**. Tests for HTTP/auth/protocol paths
require explicit features:

```sh
cargo test -p krab_core --features rest
cargo test -p krab_core --features rest protocol
cargo test -p krab_core --all-features
cargo test -p krab_macros            # trybuild compile-fail suite
```

Features: `rest`, `graphql`, `grpc`, `db`, `redis-store`, `web` (wasm).

### WASM client

```sh
wasm-pack build crates/framework/krab_client --release --target web
```

---

## Conventions that are enforced

These are not stylistic preferences — CI, reviewers, or runtime checks reject
violations.

1. **No panics in startup paths.** Boot sequences return `anyhow::Result`. No
   `unwrap()`, `expect()`, or `panic!()` in service startup or config loading.
2. **Structured logging only.** `tracing` with snake_case event names and
   OTel-aligned field keys (`http.method`, `http.status_code`, `http.route`,
   `duration_ms`, `krab.protocol`, `krab.operation`, `krab.selection_source`).
3. **Secrets via `krab_core::config::read_env_or_file()`.** Never inline. In
   `staging`/`prod`, `*_FILE` or `*_VAULT_REF` sourcing is mandatory and
   startup rejects inline values.
4. **New config knobs must be documented** in [.env.example](.env.example) and
   [docs/reference/environment.md](docs/reference/environment.md) in the same change.
5. **Migrations need `rollback_sql`** unless explicitly irreversible and
   documented. `destructive: true` migrations always require it. See
   [docs/reference/database.md](docs/reference/database.md).
6. **Zero dependency-advisory ignores.** [deny.toml](deny.toml) `ignore` is empty
   and stays empty. Fix at the crate level; do not suppress.
7. **Breaking API changes** require notes in [docs/reference/api.md](docs/reference/api.md)
   and a `CHANGELOG.md` entry, plus one minor version of deprecation warning.
8. **Write output to the right place.** Generated artifacts go under `internal/`
   or the ignored `benchmarks/` result patterns — never to the repository root.
9. **Branch prefixes**: `feat/`, `fix/`, `chore/`, `docs/`, `refactor/`,
   `security/`. Squash merge into `main`.

---

## CI gates

| Workflow | Enforces |
|---|---|
| [ops-hardening.yaml](.github/workflows/ops-hardening.yaml) | fmt, clippy `-D warnings`, rustdoc, `cargo-deny`, on-call delivery path, release certify |
| [dependency-security.yaml](.github/workflows/dependency-security.yaml) | `cargo-audit`, SBOM |
| [api-contract.yaml](.github/workflows/api-contract.yaml) | contract check, protocol parity, protocol matrix |
| [db-lifecycle.yaml](.github/workflows/db-lifecycle.yaml) | migration lifecycle, rollback sim, drift, rehearsal evidence |
| [e2e-depth.yaml](.github/workflows/e2e-depth.yaml) | multi-service E2E via docker-compose |
| [nft.yaml](.github/workflows/nft.yaml) | load/non-functional gates (`N=1` vs `N=3`), reads `benchmarks/thresholds.json` |
| [topology-matrix.yaml](.github/workflows/topology-matrix.yaml) | single vs split topology |
| [streaming-slo-gate.yaml](.github/workflows/streaming-slo-gate.yaml) | streaming SLO thresholds |
| [wasm-size.yaml](.github/workflows/wasm-size.yaml) | WASM bundle size budget |
| [release-attestation.yaml](.github/workflows/release-attestation.yaml) | provenance hashes, certification bundle |
| [rustdoc-publish.yaml](.github/workflows/rustdoc-publish.yaml) | docs publish |
| [service-smoke.yaml](.github/workflows/service-smoke.yaml) | per-service smoke |

---

## Known constraints and gotchas

- **`krab_core::Node` is `!Send`.** It uses `Rc` via the `Dynamic` variant and
  `EventListener`. Do not hold a `Node` across an `.await`. Build and consume it
  inside synchronous sections of async handlers. See
  `internal/plans/memory_notes.md`.
- **`service_frontend/build.rs` generates route registration** from
  `src/routes/*.rs`. Generated route handlers must be `async fn` returning
  `String` or `impl IntoResponse`, and their signature must match
  `Router::add_route`.
- **Feature gating is real.** A `cargo test -p krab_core` with no features
  compiles a much smaller surface than CI runs. Always match the feature set the
  gate uses before claiming a test passes.
- **Two database drivers.** `KRAB_DB_DRIVER=postgres` (default, full migration
  governance) or `sqlite`. MySQL was removed deliberately (pulled in `rsa`) —
  do not reintroduce it.
- **Forwarded headers are untrusted by default** (`KRAB_TRUST_PROXY_HEADERS=false`).
- **`benchmarks/` mixes tracked inputs with ignored results.** `thresholds.json`,
  `benchmark_config.json`, and `trend_history.csv` are tracked; `latest_summary*`,
  `*_replica_results.json`, and the evidence bundle are not.
- **`target/` and `dist/` are build output.** Never edit; never commit.
- **`.env` is local and untracked.** Copy from [.env.example](.env.example).

---

## Working agreements for agents

1. **Verify before claiming.** No change is "done" until the relevant gates have
   been run and their output captured. Record it per
   `internal/audit/VERIFICATION_EVIDENCE_LOG.md`. The
   [`krab-verify`](.claude/skills/krab-verify/SKILL.md) skill automates this.
2. **Report failures honestly.** If a gate fails or was skipped, say so with the
   output. Never summarize an unrun command as passing.
3. **Plans are governed.** Creating a plan follows
   `internal/plans/PLAN_CREATION_RULES.md`; closing one follows
   `internal/plans/PLAN_CLOSING_RULES.md`. Do not mark a plan complete without
   linked evidence. The [`krab-plan`](.claude/skills/krab-plan/SKILL.md) skill
   routes both.
4. **Changelog discipline.** Any user-visible change lands an entry under
   `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) in the same change.
5. **Docs are part of the change.** Config knob → `.env.example` +
   `docs/reference/environment.md`. Endpoint → `docs/reference/api.md`.
   Architecture decision → a new ADR in [docs/adr/](docs/adr/).
6. **Respect the public/internal boundary.** New public-facing documentation goes
   in `docs/` under the right category. Internal-only material goes in
   `internal/`. Do not add new top-level files to the repository root.
7. **Scope discipline.** This repo has extensive audit/plan history. Do not
   retroactively "fix" documents outside the requested scope; note them instead.

---

## Project skills

| Skill | Use when |
|---|---|
| [`krab-verify`](.claude/skills/krab-verify/SKILL.md) | Running gates, capturing evidence, proving a change works, release sign-off |
| [`krab-plan`](.claude/skills/krab-plan/SKILL.md) | Writing, closing, superseding, or auditing a plan under `internal/plans/` |

---

## Key references

| Topic | Document |
|---|---|
| Documentation index | [docs/README.md](docs/README.md) |
| Contribution workflow | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Release channels and promotion | [RELEASE_POLICY.md](RELEASE_POLICY.md) |
| Roadmap | [docs/roadmap.md](docs/roadmap.md) |
| API contract | [docs/reference/api.md](docs/reference/api.md) |
| Environment variables | [docs/reference/environment.md](docs/reference/environment.md) |
| Security architecture | [docs/reference/security.md](docs/reference/security.md) |
| Database + migration governance | [docs/reference/database.md](docs/reference/database.md) |
| Deployment | [docs/reference/deployment.md](docs/reference/deployment.md) |
| Architecture deep-dive | [docs/architecture/design.md](docs/architecture/design.md) |
| Service composition / topology | [docs/architecture/service_composition.md](docs/architecture/service_composition.md) |
| Signal threading constraints | [docs/architecture/signal_safety.md](docs/architecture/signal_safety.md) |
| Production readiness gates | [docs/operations/production_readiness.md](docs/operations/production_readiness.md) |
| On-call runbook | [docs/operations/oncall_playbook.md](docs/operations/oncall_playbook.md) |
| DB rollback runbook | [docs/operations/db_rollback_runbook.md](docs/operations/db_rollback_runbook.md) |
