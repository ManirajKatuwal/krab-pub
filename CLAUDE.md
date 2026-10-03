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

- Version: `0.6.0` (workspace-wide, `[workspace.package]` in `Cargo.toml`; see
  `docs/operations/release_0_6_0_checklist.md`). `next_version` is `0.7.0`.
  Note
  that `0.3.0` was tagged and released on GitHub but never reached the registry,
  so crates.io goes `0.2.0` → `0.4.0` — the missing number is deliberate.
  `[workspace.metadata.krab] next_version` must always be one ahead of
  `version`; `scripts/check_workspace_layout.py` fails the build otherwise.
- Edition: 2021, Rust stable 1.89+ (`rust-version` in `Cargo.toml` is the
  measured floor of the resolved graph — `async-graphql` 7.2 and `time` 0.3.47 —
  not a Krab choice; it said 1.75 through 0.4.0 and was never true)
- License: MIT

---

## Repository layout

```
crates/framework/
  krab_core/        Shared runtime: config, http*, db, telemetry, resilience,
                    signal, protocol, render_policy, isr, i18n, ws, server_fn
  krab_macros/      Proc macros: view!, #[island], #[server]  (+ trybuild tests)
  krab_client/      WASM island hydration runtime (browser)
crates/tooling/
  krab_cli/         `krab` CLI: dev workflow, generators, governance, release ops
  krab_orchestrator/ Multi-process service runner driven by krab.toml
services/
  service_auth/     REST auth (JWT/OIDC)              → :3001
  service_users/    GraphQL + Postgres/SQLite         → :3002
  service_frontend/ SSR + islands                     → :3000
  service_frontend_islands/ The frontend's islands: linked by
                    service_frontend for SSR, built by wasm-pack as its
                    browser bundle (library, publish = false)
  service_users_split/ Split-topology reference svc   → :3207
examples/reference_apps/
  islands_rpc/      Vendored reference app (islands + server functions)
```

Eleven workspace members; `scripts/check_workspace_layout.py` counts them.

Supporting directories:

| Path | Contents | Tracked |
|---|---|---|
| [docs/](docs/) | All public documentation — see [docs/README.md](docs/README.md) | Yes |
| [benchmarks/](benchmarks/) | NFT thresholds, benchmark config, trend history | Inputs yes, results no |
| [scripts/](scripts/) | Python NFT/benchmark/evidence tooling, `check_health.ps1` | Yes |
| [monitoring/](monitoring/) | Prometheus config, alert rules, Grafana dashboard | Yes |
| [docker/](docker/) | Postgres init scripts (rendered NFT compose is generated, ignored) | Yes |
| [examples/](examples/) | One vendored reference app (`islands_rpc`, a workspace member) plus five generated-track READMEs | Yes |
| [.github/workflows/](.github/workflows/) | 14 CI gate workflows | Yes |
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

`internal/` exists only in the maintainer's working copy — it is untracked, so
a fresh clone does not have it. **If `internal/` is absent in your checkout,
skip every workflow below that depends on it** (plan governance, evidence
ledger) and rely on the public gates instead. **Never make `internal/` the
only home for something a user needs** — public behaviour belongs in `docs/`.

---

## Documentation map

Public documentation is organised by purpose:

| Directory | Holds | Examples |
|---|---|---|
| [docs/guides/](docs/guides/) | Task-oriented walkthroughs | `getting_started.md`, `ide_setup.md`, `migration_guide.md`, `dev_workflow.md`, `reference_apps.md`, `why_krab.md` |
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
cargo run --bin service_users_split # :3207 (split-topology reference)
cargo run --bin krab_orchestrator   # all of the above, per krab.toml
```

### CLI governance commands

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
cargo run -p krab_cli -- security dependency-gate --diagnostics
```

Which of these CI runs: `contract check`, `contract protocol-check`, the four
`db` commands, `release certify` and `security dependency-gate` appear in a
workflow. **`doctor`, `env-check`, `topology doctor` and `release check` are
not invoked by any workflow** — run them yourself.

`contract check|protocol-check`, `db lifecycle|rollback|drift|rehearsal` and
`release check|certify` are **framework-only**: they validate this repo's own
reference services and refuse to run (non-zero, one explanatory message)
unless `./Cargo.toml` lists `crates/framework/krab_core` as a workspace member.
`doctor`, `env-check`, `topology doctor` and `security dependency-gate` work in
any project. `doctor` and `env-check` load `./.env` first; variables already in
the process environment win. `db lifecycle|rollback|drift|rehearsal` all fail
without a reachable Postgres (`KRAB_TEST_DATABASE_URL`, then `DATABASE_URL`).

Generated output defaults, all under ignored paths. `<root>` is the artifact
root: `KRAB_ARTIFACT_DIR` if set and non-empty; else `internal/audit/` if that
directory exists (a deprecated fallback that warns, removed in 0.7.0 — so in a
maintainer checkout it is still `internal/audit/`); else `.krab/`.

| Command | Writes to (without `--out`) |
|---|---|
| `krab db rehearsal` | `<root>/evidence/rollback-rehearsal-evidence.txt` |
| `krab release certify` | `<root>/release-certify/local/` |
| `krab bootstrap` / `krab_orchestrator` (service logs) | `<root>/orchestrator/` |
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

Features: `rest`, `graphql`, `grpc-semantics`, `auth`, `db-postgres`,
`db-sqlite`, `redis-store`, `web` (wasm). The aliases `db` and `grpc` (and the
module alias `krab_core::grpc`) were removed in 0.6.0 — do not reintroduce them.

Tests that need a live backend skip loudly when it is absent, and fail instead
when told they are a gate:

| Backend | Point at it with | Make absence fail with |
|---|---|---|
| Postgres (`db_tests`, `--features db-postgres`) | `KRAB_TEST_DATABASE_URL` (then `DATABASE_URL`) | `KRAB_REQUIRE_DB_TESTS=1` |
| Redis (`RedisStore`, `--features redis-store`) | `KRAB_TEST_REDIS_URL` | `KRAB_REQUIRE_REDIS_TESTS=1` |

A skipped suite is not a passing suite; say which ones actually ran.

The Windows host builds and tests the whole workspace, C dependencies included
(`ring`, `libsqlite3-sys`; verified 2026-09-30). The publish dry-run still
belongs in a container on an LF checkout.

`grpc-semantics` is **not** a gRPC transport — it is status-code and
`grpc-timeout` header vocabulary for a gateway. `tonic`/`prost` are not
dependencies and `ProtocolKind::parse("grpc")` returns `None`. See
[ADR 0007](docs/adr/0007-grpc-feature-disposition.md).

### WASM client

```sh
wasm-pack build crates/framework/krab_client --release --target web -- --features web
```

`krab_client`'s features are `web` (the hydration runtime and the client
router's browser half) and `debug` (verbose console tracing of the hydration
walk). `web` is the **default**, so the `--` suffix above is redundant — it is
written out because this command shipped a stub for the entire 0.1–0.2 line,
when `web` was opt-in and nothing asked for it. A build without `web` is not a
smaller runtime, it is a `hydrate()` that logs one line and returns: ~15 KB
instead of ~165 KB. The `demo-islands` feature (bundled
`Counter`/`Toggle`/`Likes`) was removed in 0.6.0.

`krab_client` itself contains no islands, so the bundle a page loads is always
an application crate built for wasm32. `service_frontend`'s is
`services/service_frontend_islands` — `krab.toml`'s `client_package`:

```sh
wasm-pack build services/service_frontend_islands --release --target web --out-dir ../../dist/pkg -- --features web
```

`#[island]` picks its half from the **defining crate's** `web` feature: the
server links `service_frontend_islands` without it (SSR wrapper markup), the
wasm build turns it on (hydrating half + registration).

---

## Conventions that are enforced

These are not stylistic preferences — CI, reviewers, or runtime checks reject
violations.

1. **No panics in startup paths.** Boot sequences return `anyhow::Result`. No
   `unwrap()`, `expect()`, or `panic!()` in service startup or config loading.
   (`KrabConfig::from_env`, which panicked, is gone — use `from_env_checked`.)
2. **Structured logging only.** `tracing` with snake_case event names and
   OTel-aligned field keys (`http.method`, `http.status_code`, `http.route`,
   `duration_ms`, `krab.protocol`, `krab.operation`, `krab.selection_source`).
3. **Secrets via `krab_core::config::read_env_or_file()`.** Never inline. In
   `prod`, startup rejects inline values and `*_FILE` sourcing is mandatory; in
   `staging`, inline values are a warning. `*_VAULT_REF` is recognised but
   always refused — there is no runtime vault resolver, so materialise the
   secret to a file before startup.
4. **New config knobs must be documented** in [.env.example](.env.example) and
   [docs/reference/environment.md](docs/reference/environment.md) in the same change.
5. **Migrations need `rollback_sql`** unless explicitly irreversible and
   documented. `destructive: true` migrations always require it. See
   [docs/reference/database.md](docs/reference/database.md).
6. **Dependency advisories are fixed, not suppressed.** [deny.toml](deny.toml)
   has no `ignore` list and stays that way — fix at the crate level, usually a
   `cargo update -p <crate>`. There is exactly **one** standing exception, in
   [.cargo/audit.toml](.cargo/audit.toml): `RUSTSEC-2023-0071` (`rsa`), reached
   only through `sqlx-mysql`, which `sqlx-macros-core` depends on
   unconditionally. Krab compiles Postgres and SQLite drivers only, so the code
   path is unreachable, and there is no upstream fix. Adding a second entry to
   either file requires an ADR.
7. **Breaking API changes** require notes in [docs/reference/api.md](docs/reference/api.md)
   and a `CHANGELOG.md` entry, plus one minor version of deprecation warning.
8. **Write output to the right place.** Generated artifacts go under the
   artifact root (`.krab/`, or `internal/audit/` in a maintainer checkout) or
   the ignored `benchmarks/` result patterns — never to the repository root.
9. **Every public `krab_core` item is documented.** `#![warn(missing_docs)]`
   in `krab_core/src/lib.rs` is an error under `clippy -D warnings`, so a new
   `pub` item without rustdoc fails the gate.
10. **Branch prefixes**: `feat/`, `fix/`, `chore/`, `docs/`, `refactor/`,
   `security/`. Squash merge into `main`.

---

## CI gates

No GitHub Actions run has yet succeeded on either repository (Actions billing).
These workflows define the gate surface; until they execute, the same commands
are run in a container or on the host and recorded as evidence, never as a CI
link.

| Workflow | Enforces |
|---|---|
| [ops-hardening.yaml](.github/workflows/ops-hardening.yaml) | workspace layout, inter-crate version pinning, fmt, clippy `-D warnings`, rustdoc, `cargo-deny`, on-call delivery path, publish dry-run, release certify |
| [generated-project.yaml](.github/workflows/generated-project.yaml) | `krab new` output builds, tests, clippy `-D warnings`, `fmt --check` — all five templates (`default`, `saas`, `edge-ssr`, `event-stream`, `fullstack`) — and so does `krab gen service` output (single and multi mode) |
| [reference-app.yaml](.github/workflows/reference-app.yaml) | `examples/reference_apps/islands_rpc` builds, tests, and lints on native **and** `wasm32`, its WASM bundle is produced, and the `krab_client` browser suites run in headless Chrome |
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
  `String` or `impl IntoResponse`, and their signature must satisfy
  `axum::routing::get`. (This said `Router::add_route` — a method on the removed
  `krab_server` crate — from the day it was written. `build.rs` has always
  emitted `axum::Router` registration. See
  [ADR 0005](docs/adr/0005-krab-server-disposition.md).)
- **Feature gating is real.** A `cargo test -p krab_core` with no features
  compiles a much smaller surface than CI runs. Always match the feature set the
  gate uses before claiming a test passes.
- **Two database drivers.** `KRAB_DB_DRIVER=postgres` (default, full migration
  governance) or `sqlite`. MySQL was removed deliberately (pulled in `rsa`) —
  do not reintroduce it. Driver selection (`DbDriver`, `resolve_db_driver`)
  lives in `krab_core::db`; the Cargo features are `db-postgres` and
  `db-sqlite` (the `db` alias was removed in 0.6.0). Postgres migrations run
  through `run_versioned_migrations`; `run_migrations` is gone. Compiling a
  driver in and selecting one at runtime are separate — enable both features
  and choose per environment if you need to.
- **Forwarded headers are untrusted by default** (`KRAB_TRUST_PROXY_HEADERS=false`).
- **`benchmarks/` mixes tracked inputs with ignored results**, and not every
  result is ignored. Tracked: `thresholds.json`, `benchmark_config.json`,
  `trend_history.csv`, and — deliberately — the committed result snapshots
  `external_results.json`, `external_summary.md`, and
  `targeted_hardening_results.json`. Ignored: `latest_summary*`,
  `*_replica_results.json`, `shared_state_validation.json`, and the evidence
  bundle. [`benchmarks/README.md`](benchmarks/README.md) holds the authoritative
  table — check it before adding an artifact, and add a matching `.gitignore`
  rule in the same change if the new artifact is meant to be generated.
- **`target/` and `dist/` are build output.** Never edit; never commit.
- **`.env` is local and untracked.** Copy from [.env.example](.env.example).

---

## Working agreements for agents

1. **Verify before claiming.** No change is "done" until the relevant gates have
   been run and their output captured. Maintainer checkouts record it per
   `internal/audit/VERIFICATION_EVIDENCE_LOG.md` (skip if `internal/` is absent);
   the maintainer-local `krab-verify` skill automates this.
2. **Report failures honestly.** If a gate fails or was skipped, say so with the
   output. Never summarize an unrun command as passing.
3. **Plans are governed** (maintainer checkouts only — skip if `internal/` is
   absent). Creating a plan follows `internal/plans/PLAN_CREATION_RULES.md`;
   closing one follows `internal/plans/PLAN_CLOSING_RULES.md`. Do not mark a
   plan complete without linked evidence. The maintainer-local `krab-plan`
   skill routes both.
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

Maintainer-local, under untracked `.claude/skills/` — present only in the
maintainer's working copy. If your checkout does not have them, run the
verification commands above directly.

| Skill | Use when |
|---|---|
| `krab-verify` | Running gates, capturing evidence, proving a change works, release sign-off |
| `krab-plan` | Writing, closing, superseding, or auditing a plan under `internal/plans/` |

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
