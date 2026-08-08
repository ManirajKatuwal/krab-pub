# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/) and
[Semantic Versioning](https://semver.org/). Entries are newest-first.

Change categories: **Added**, **Changed**, **Deprecated**, **Removed**,
**Fixed**, **Security**, **Governance**.

Every user-visible change lands an entry under `[Unreleased]` in the same commit
as the change itself. Entries describe outcomes, not tasks or plan phases.
Release requirements are defined in [`RELEASE_POLICY.md`](RELEASE_POLICY.md).

---

## [Unreleased]

Covers work merged after `0.1.1` (2026-03-11) through commit `bac72c5`
(2026-06-10). Not yet released or version-tagged.

### Added

- **The workspace is publishable.** Every inter-crate dependency now carries a
  `version` alongside its `path`, declared once in `[workspace.dependencies]`.
  Previously all six framework and tooling crates were path-only and
  `cargo publish` rejected them outright, so Krab was consumable only by cloning
  this repository. `cargo publish --workspace --dry-run` now exits 0 and is
  enforced by the `publish-dry-run` job in `ops-hardening`. Publication order
  and preconditions are documented in
  [`RELEASE_POLICY.md`](RELEASE_POLICY.md#crate-publication).
- **`krab_cli` installs a binary named `krab`.** Added an explicit `[[bin]]`
  section. The binary previously inherited the package name `krab_cli`, so
  `cargo install krab_cli` produced a command that matched none of the
  documented invocations (`krab doctor`, `krab new`, `krab release certify`).
  The package cannot be renamed — the crates.io name `krab` was registered in
  2023 by an unrelated crate.
- **Installation section in [`README.md`](README.md)** covering `cargo add` and
  `cargo install`, with the per-crate breakdown and the `krab_core` feature
  list. No documentation previously showed adding Krab as a dependency.
- **Inter-crate version-pin check** in `scripts/check_workspace_layout.py`:
  every `krab_*` entry in `[workspace.dependencies]` must carry a `path` and a
  `version` matching `[workspace.package] version`. Cargo has no
  `version.workspace = true` for workspace dependencies, so the value is
  duplicated by necessity; without this check a stale pin surfaces only at
  publish time.
- **`view!` accepts hyphenated, namespaced, and keyword names.** Tag and
  attribute names now parse as `Ident (('-' | ':') (Ident | LitInt))*` with
  raw-identifier support, so `data-testid`, `aria-label`, `xlink:href`,
  `<my-widget>`, `<input type="text">`, and `<label for="email">` all work.
  Names previously parsed as a bare `syn::Ident`, which cannot contain `-` or
  `:` and rejects Rust keywords — which is why `#[island]` builds its
  `data-island` / `data-krab-boundary-*` wrapper by constructing
  `krab_core::Attribute` values directly, and why the reference frontend
  hand-writes HTML strings for island markup.
- **`krab new --path-deps <KRAB_REPO_ROOT>`** points a generated project at a
  local Krab checkout instead of crates.io. Required by the new
  `generated-project` gate, which must build scaffolded output before that
  version is published.
- **`generated-project` CI workflow** builds, tests, clippy-checks, and
  format-checks a real `krab new` output for all four templates. The primary
  onboarding path was previously unguarded — the only tests asserted that files
  existed and contained given substrings.
- **`krab --version`.** The CLI had no version flag.
- **SQLite is a framework driver.** `krab_core`'s `db` feature is split into
  `db-postgres` and `db-sqlite`, and driver selection — `DbDriver`,
  `resolve_db_driver`, `default_db_url_for_driver` — moves from
  `services/service_users` into `krab_core::db`. `krab_core`'s `sqlx`
  dependency previously enabled `postgres` unconditionally and nothing else, so
  `KRAB_DB_DRIVER=postgres|sqlite` was documented as a framework choice that
  only a reference application could actually make. Both driver features are
  now compiled independently in CI. `db` remains a deprecated alias for
  `db-postgres` for one minor version.
- **`krab_core` HTTP layer split** into focused modules: `http_auth`,
  `http_error`, `http_headers`, `http_observability`, `http_protocol`,
  `http_runtime`, and `http_security`, alongside the existing `http`.
- **GraphQL and gRPC protocol modules** in `krab_core` (`graphql.rs`, `grpc.rs`).
  GraphQL is a full integration via `async-graphql`. The `grpc` feature provides
  gRPC **status-code and metadata semantics** for protocol negotiation — it does
  not bundle a transport.
- **`service_contract` and `render_policy` modules** in `krab_core`.
- **`krab_cli` restructured** into dedicated modules — `dev_workflow`, `doctor`,
  `generator`, `governance`, `project_model`, `project_template`, `release_ops`,
  `topology` — with new commands:
  - `krab doctor --diagnostics --strict` — aggregated workspace health checks
  - `krab release check` / `krab release certify --out <dir> --json` — release
    pre-flight and evidence bundle generation
  - `krab topology doctor` / `krab topology split <domain>` — topology hygiene
    checks and split-service extraction scaffolding
  - `krab new <name> --template <t>` — project templates
  - `krab bootstrap` — one-command local stack (build + orchestrator)
- **`krab_orchestrator` restructured** into `configuration`, `process_runtime`,
  and `watch_runtime` modules.
- **`service_users_split`** reference service for split-service topology
  (port `3207`), registered in the workspace and `krab.toml`.
- **`krab_macros` compile-fail test suite** (trybuild) covering `empty_view`,
  `island_generic`, `mismatched_closing_tag`, `server_invalid_attr`,
  `server_invalid_return`, `server_method_self`, and `server_not_async`, plus an
  island expansion test.
- **New CI workflows**: `release-attestation.yaml` (provenance hashes +
  certification evidence), `streaming-slo-gate.yaml`, `topology-matrix.yaml`.
- **Architecture Decision Records** in `docs/adr/`:
  - `0001-hydration-markers.md`
  - `0002-render-policy.md`
  - `0003-server-functions-public-endpoints.md`
- **New documentation**: hydration, render policy, server functions, service
  composition, migration guide, reference apps, why-krab, IDE setup, and the
  FaaS platform review. See the reorganisation note under **Changed** for their
  current locations.
- **Reference application tracks** under `examples/reference_apps/`:
  `content_site`, `edge_rendered`, `event_stream`, `saas_dashboard`,
  `split_service`.
- **PostgreSQL container bootstrap** — `docker/postgres/init/01-create-users-db.sh`.
- **`.cargo/audit.toml`** and **`.dockerignore`**.
- **Agent and governance documentation**: `CLAUDE.md`,
  `internal/audit/VERIFICATION_EVIDENCE_LOG.md`,
  `internal/plans/PLAN_CREATION_RULES.md`,
  `internal/plans/PLAN_CLOSING_RULES.md`, and project skills under
  `.claude/skills/`.
- **Documentation indexes**: `docs/README.md` (public documentation map) and
  `internal/README.md` (internal boundary and generated-artifact map).
- **Per-crate `README.md` for all six publishable crates** — `krab_core`,
  `krab_client`, `krab_macros`, `krab_server`, `krab_cli`, `krab_orchestrator`.
- **crates.io publish metadata** on those crates: `description`, `keywords`,
  `categories`, `documentation`, and `readme`. `krab_cli` and
  `krab_orchestrator` previously carried no description at all, which blocks
  publishing outright. The four reference services under `services/` are now
  explicitly `publish = false`.
- **WebSocket, RPC, and crawler endpoints documented** in
  `docs/reference/api.md`: `POST /api/v1/rpc`, `GET /api/ws/chat`,
  `POST /api/ws/publish`, `GET /{locale}`, `/robots.txt`, `/sitemap.xml`, and
  `/api/hmr`.

### Changed

- **Release profile split: server binaries now build for speed, the WASM client
  for size.** `[profile.release]` moves from `opt-level = "z"` to
  `opt-level = 3`, with `opt-level = "z"` scoped to `krab_client` via
  `[profile.release.package.krab_client]`. Size optimisation had applied
  workspace-wide, so every service binary traded throughput to shrink an
  artifact that is never shipped over a network — while the browser bundle,
  where size genuinely matters, is separately gated at 500 KB raw / 150 KB gzip
  and keeps `"z"` plus `wasm-opt -Oz`. Downstream consumers should expect
  larger, faster release binaries. Measured WASM impact: the bundle grows from
  1,422 B to 16,420 B raw (897 B → 7,178 B gzip) — a 11.5× relative increase
  that is still **3% of the 500 KB raw budget**, because the per-package
  override applies to `krab_client` itself but not to its dependencies. If the
  client bundle ever approaches the budget, move the WASM build to a dedicated
  `[profile.wasm-release]` so the whole dependency graph is size-optimised.
  `panic` is
  deliberately left at `unwind`: Tower and Axum contain a panicking request to
  its own connection, whereas `abort` would terminate the process and every
  in-flight request with it.
- **CI now runs the test suite.** `ops-hardening.yaml` gained
  `cargo test --workspace` and `cargo test -p krab_core --all-features`. No
  workflow previously ran either; the only test invocations were
  `-p service_users`, `-p service_frontend`, and
  `cargo test -p krab_core --features rest protocol` — where `protocol` is a
  test-name filter, not a second feature. `krab_core`'s auth, db, api, and
  server-function suites, and every doc example, therefore ran only on
  developer machines. The second step is scoped to `krab_core` rather than
  `--workspace --all-features` on purpose: the latter would enable
  `service_frontend`'s `nft` feature, whose 6 ms p95 latency assertions are
  only meaningful on the dedicated runners in `nft.yaml`. Scoping it to
  `krab_core` is also the only thing that compiles its `grpc` and `web` code
  paths at all — no workspace member enables either feature.
- `service_frontend` render policy behaviour updated (`src/render_policy.rs`).
- `krab_core` public module surface (`lib.rs`) re-exported to match the HTTP and
  protocol module split.
- `krab_cli` release operations reworked to emit machine-readable JSON summaries.
- `docker-compose.yml` and `docker-compose.nft.yaml` environment bootstrap
  reworked; `composed.nft.rendered.yaml` added as the rendered NFT composition.
- `Dockerfile.service` and `check_health.ps1` updated for the current service set.
- `monitoring/prometheus.yml` scrape targets updated.
- `.env.example` expanded for the new configuration surface.
- `README.md` rewritten with the current architecture, feature, and configuration
  reference.
- `CONTRIBUTING.md` updated with the current CI gate table and engineering
  standards.
- `deny.toml` policy updated.
- **Repository reorganised** around a public/internal boundary:
  - `docs/` is now split by purpose into `guides/`, `reference/`,
    `architecture/`, `operations/`, and `adr/`, indexed by `docs/README.md`.
    `docs/API.md` → `docs/reference/api.md`, `docs/security.md` →
    `docs/reference/security.md`, and so on for every public document.
  - Genuinely public planning documents were promoted out of `plans/`:
    `environment_template.md` → `docs/reference/environment.md`,
    `01_vision_and_philosophy.md` → `docs/architecture/vision.md`,
    `02_architecture_design.md` → `docs/architecture/design.md`,
    `03_roadmap.md` → `docs/roadmap.md`, `08_production_readiness.md`,
    `oncall_playbook.md`, `db_rollback_runbook.md`, `slo_alerts.md`, and
    `api_governance.md` → `docs/operations/`.
  - All internal material moved under a single `internal/` tree
    (`plans/`, `audit/`, `wiki/`, `reports/`), replacing six separate
    `.gitignore` rules with one.
  - `plans/load_test_artifacts/` → `benchmarks/`, so NFT thresholds and
    benchmark config are tracked CI inputs rather than ignored planning files.
  - Root reduced from 18 files to 14: `check_health.ps1` → `scripts/`,
    `composed.nft.rendered.yaml` → `docker/`, `AUDIT.md` →
    `internal/reports/`, `rollback-rehearsal-evidence.txt` →
    `internal/audit/evidence/`.
  - 841 markdown cross-links rewritten to match the new depths.
- `.gitignore` consolidated: one `internal/` rule for all internal
  documentation, plus `__pycache__/`, generated benchmark results, and the
  rendered NFT compose file.
- CI and tooling output paths repointed: `krab release certify` →
  `internal/audit/release-certify/`, orchestrator logs →
  `internal/audit/orchestrator/`, `krab db rehearsal` →
  `internal/audit/evidence/`, `krab docs` → `docs/guides/dev_workflow.md`.
  The seven NFT scripts now read and write `benchmarks/`.

### Fixed

- **An unset `KRAB_DB_DRIVER` now selects Postgres, not SQLite.**
  `.env.example`, [`docs/reference/environment.md`](docs/reference/environment.md),
  and `CLAUDE.md` all documented `postgres` as the default; the code in
  `service_users` defaulted to `sqlite`. Falling back to the driver *without*
  migration governance because a variable was unset is the more dangerous
  direction, so the code now matches the documentation. Every CI and compose
  configuration already set the variable explicitly and is unaffected.
- **The `edge-ssr` template's ISR cache is actually used.** It was constructed
  into `AppState` and never read, which failed the `-D warnings` clippy that the
  generated project's own CI workflow runs. `/` now serves through the cache
  with stale-while-revalidate, and the starter-scope note no longer disclaims
  the ISR serving the template performs.
- **`krab new` output now compiles.** Three independent defects in the generated
  `Cargo.toml`, none caught by the substring-matching template tests:
  the `krab_core` version was a hard-coded `"0.1.0"` that had fallen behind the
  workspace's `0.1.1`; the `saas` template emitted
  `features = ["db, rest"]` — a single feature literally named `db, rest`,
  which Cargo rejects; and `krab_macros` was absent entirely, putting `view!`,
  `#[island]`, and `#[server]` out of reach of a scaffolded project. The version
  is now derived from the CLI's own package version, features are rendered from
  a list, and the template tests parse the manifest rather than grepping it.
- **A scaffolded project passes its own generated CI.** The `default` template's
  route registration exceeded 100 columns once the project name was
  substituted, so `cargo fmt --all --check` — which the generated CI workflow
  runs — failed on the first commit of every new project. The template now uses
  named handler functions whose width does not depend on the project name.
- **`view!` reports capitalised tags as an error.** `<MyComponent/>` previously
  emitted the literal markup `<MyComponent>`, which no browser renders, with no
  diagnostic at any stage. `view!` has no component composition; the error names
  the working alternative. See
  [ADR 0006](docs/adr/0006-view-component-composition.md).
- **`collect_server_fns!` now compiles.** The macro expanded to
  `paste::paste! { ... }`, but `paste` was not a dependency of `krab_core` or
  any workspace crate, so the documented registration pattern in
  `docs/reference/server_functions.md` failed at every call site. `#[server]`
  now emits a hidden marker type implementing the new
  `krab_core::server_fn::ServerFn` trait, and the macro resolves each
  function's name, URL, and dispatch handler through it — no identifier
  concatenation and no new dependency. (`paste` is unmaintained per
  RUSTSEC-2024-0436, so adding it was not an option.) The marker is declared as
  `struct {name} {}` so it occupies only the type namespace and does not
  collide with the function it is named after.
- **`krab gen component` and `krab gen route` generated code that could not
  compile.** Both templates were written against another framework's API,
  referencing `krab_core::prelude`, `#[component]`, `#[route(...)]`, and
  `impl IntoView` — none of which exist in Krab. Components now return
  `krab_core::Node`, and routes emit `pub async fn handler()`, matching the
  discovery contract in `services/service_frontend/build.rs`. Both templates
  are now unit-tested, including an assertion that the phantom API cannot
  reappear.
- **Islands example in `README.md` and `docs/architecture/design.md`
  corrected.** It showed `pub fn Counter(initial: i32) -> impl View`; there is
  no `View` trait, and `#[island]` requires exactly one serialisable props
  struct and a `krab_core::Node` return. The published example is now compiled
  and asserted by `readme_counter_example_renders_on_the_server` in
  `crates/framework/krab_macros/tests/island_expansion.rs`.
- **All 11 rustdoc examples are now compiled instead of skipped.** Every
  example carried a ```` ```rust,ignore ```` fence, so none were ever checked.
  Un-ignoring them surfaced three further stale references, now fixed:
  `krab_core::ws::WsHandler` (does not exist), `PropagationHeaders::inject`
  (the method is `inject_into_headers`, with `as_header_pairs` for
  builder-style clients), and `HeadContext::render` (it is `render_tags`).
- **Documented `#[server]`'s dependency requirements.** The expansion
  references `axum`, `serde`, `serde_json`, and `krab_core` by path, so all
  four must be direct dependencies of the calling crate, and `krab_core` must
  carry the `rest` feature — the expansion implements
  `krab_core::server_fn::ServerFn`, which is gated behind it. A proc macro
  cannot observe the calling crate's feature flags, so neither requirement can
  be checked at expansion time; both were previously undocumented and only
  discoverable from a macro-expansion error.
- **Data-loading section of `docs/architecture/design.md` corrected.** It
  documented a `loader` convention and `[id]`-style dynamic segments as
  existing behaviour; neither is implemented. The section now shows the real
  `pub async fn handler()` contract and marks the loader pattern explicitly as
  a design goal.
- **Workspace version corrected to `0.1.1`.** `[workspace.package]` still read
  `0.1.0` even though `0.1.1` was recorded as released on 2026-03-11. All
  workspace members now inherit it, including `krab_cli` and
  `krab_orchestrator`, which had hardcoded `0.1.0`.
- **512 broken relative links repaired** across `internal/` documentation (534 →
  22), left dangling by the `crates/` and `services/` reorganisation. Every
  rewrite was verified to resolve to an existing file. The 22 that remain are
  intentional: 12 name a proposed `http/` submodule layout the implementation
  did not adopt, and 10 name files deleted or renamed after the dated audit that
  cites them. Both groups are annotated in place.
- `krab db rehearsal` now creates the parent directory of its `--out` path
  before writing, instead of failing when the directory does not yet exist.
- `scripts/__pycache__/*.pyc` removed from version control and `__pycache__/`
  added to `.gitignore`.
- Missing `sqlx` trait bounds on manual `FromRow` implementations (`3ec3e23`).
- Compose environment bootstrap and WASM `tokio` target gating in CI (`bec6643`).
- E2E teardown diagnostics added for compose env interpolation failures
  (`3c8e3c0`).

### Security

- **Five dependency advisories remediated** by lockfile update, restoring both
  `cargo audit` and `cargo deny check` to green:

  | Advisory | Crate | Resolution |
  |---|---|---|
  | RUSTSEC-2026-0185 | `quinn-proto` (via `reqwest`) | 0.11.14 → 0.11.16 — 7.5 High, remote memory exhaustion from unbounded out-of-order stream reassembly |
  | RUSTSEC-2026-0205 | `scc` (via `serial_test`) | removed — `serial_test` 3.4.0 → 3.5.0 no longer depends on it |
  | RUSTSEC-2026-0190 | `anyhow` | 1.0.102 → 1.0.104 — unsoundness in `Error::downcast_mut()` |
  | RUSTSEC-2026-0221 | `event-listener` | 5.4.1 → 5.4.2 — `!Send` tags crossing thread boundaries via `StackSlot` |
  | yanked | `spin` | 0.9.8 → 0.9.9 |

  No source changes were required; no advisory was suppressed.
- Runtime configuration hardening across services (`d54db0a`).
- `docs/security.md` updated for the current threat model and secret-sourcing
  behaviour.
- **`KRAB_CSRF_ENABLED`, `KRAB_AUTH_COOKIE_SESSION_ENABLED`, and
  `KRAB_AUTH_REQUIRE_TENANT_CLAIM` are now documented.** All three are
  security-relevant, default to off, and were previously undocumented in both
  `.env.example` and the environment reference.

### Governance

- **Workspace layout is now a CI gate.** `scripts/check_workspace_layout.py`
  runs in `ops-hardening` and fails on a workspace member outside
  `crates/framework/`, `crates/tooling/`, or `services/`; on a member whose
  `Cargo.toml` is missing; or on a crate directory at the repository root — the
  last catching a stray crate before it reaches `members`. The layout standard
  had been convention-only since the reorg, so nothing stopped a root-level
  crate from landing.
- **ADR 0004 — Protocol selection by explicit endpoint.** Records the resolution
  order the code actually implements (allowed set → route family → gated client
  header → service default) and why header-driven negotiation stays off by
  default: the adapter determines the authorization, rate-limit, and audit
  surface, so a client able to steer the adapter can steer the policy applied to
  it. Two planning documents had specified contradictory selection models since
  the feature was designed, and neither matched the implementation.
- Release certification evidence is now generated in CI by
  `krab release certify` and uploaded as a workflow artifact.
- Provenance hashes (`Cargo.lock`, `.env.example`) recorded by
  `release-attestation.yaml`.
- **Rule 6 in `CLAUDE.md` reconciled with reality.** It claimed zero
  dependency-advisory ignores; `.cargo/audit.toml` has always carried one
  (`RUSTSEC-2023-0071`, `rsa` reached only through `sqlx-mysql`, which
  `sqlx-macros-core` depends on unconditionally). The exception is now stated
  explicitly with its justification, and a second entry requires an ADR.
- **Correction to the `[0.1.0]` entry below.** That release recorded "`rsa`
  crate entirely removed from the dependency tree" and "`deny.toml` `ignore`
  array emptied — zero advisory exceptions". The second is still true. The
  first is not: `rsa 0.9.10` is present in `Cargo.lock` today, resolved through
  `sqlx-macros-core` → `sqlx-mysql` regardless of which drivers are enabled.
  Removing MySQL as a *supported driver* did not remove the crate from the
  *resolution graph*. Released entries are not rewritten, so the correction is
  recorded here.
- **The `2026-06-04` pre-release GO sign-off is re-opened as NO-GO.** Its cited
  certification bundle does not exist, advisories broke the gates after it was
  recorded, and the tree has since been reorganised. See §6 of
  `internal/reports/PRE_RELEASE_AUDIT_REPORT.md`.

> **Verification status (2026-08-07).** Partial. `cargo fmt --all --check`,
> `cargo audit`, and `cargo deny check advisories licenses bans sources` all
> pass at HEAD — bundle:
> [`internal/audit/evidence/2026-08-07_remediation/`](internal/audit/evidence/2026-08-07_remediation).
>
> Everything requiring a linker is **unrun**: `cargo test --workspace`, the
> feature-gated `krab_core` suites, `cargo clippy --all-targets`, `cargo doc`,
> and every `krab` governance command. The current machine has no MSVC C++
> Build Tools, so proc-macro and binary targets cannot link. This is an
> environment gap, not a code failure, but it means **this section is a record
> of merged changes, not an attestation that all gates are green at HEAD.** See
> [`internal/audit/VERIFICATION_EVIDENCE_LOG.md`](internal/audit/VERIFICATION_EVIDENCE_LOG.md) §7.

---

## [0.1.1] - 2026-03-11

### Added

- Protocol flexibility rollout completion across auth/frontend/CLI:
  - `service_auth` capability endpoint: `GET /api/v1/auth/capabilities`.
  - REST-only auth guardrails tests for GraphQL/RPC non-exposure paths.
  - `krab_cli` protocol-aware scaffolding flags:
    `krab gen service --exposure-mode ... --protocols ... --topology ...`
  - `krab contract protocol-check` command for parity/resolver validation.
- New public documentation: `docs/protocol_flexibility.md`.

### Changed

- `krab_core::http` auth open-path allowlist includes `/api/v1/auth/capabilities`.
- Tracing includes additional protocol attributes: `krab.protocol`,
  `krab.operation`, `krab.selection_source`.
- Environment template expanded with `KRAB_PROTOCOL_*` configuration guidance.
- Deployment and API docs updated with capability endpoint + split-topology notes.

### Governance

- Added protocol parity and exposure-mode policy section to
  `plans/api_governance.md`.

---

## [0.1.0] - 2026-03-06

### Added

- Multi-service NFT gate with single/scale (`N=1` vs `N=3`) validation.
- SLO burn-rate alert linkage and on-call mapping.
- Rustdoc CI gate and docs publish workflow.
- Centralized `read_env_or_file` secret sourcing utility in `krab_core::config`.
- `env_non_empty` helper for safe non-empty environment variable reads.
- SQLite database driver support (`KRAB_DB_DRIVER=sqlite`) with full schema
  bootstrap.
- Feature maturity closure items:
  - Route-level middleware chaining for file-based routes.
  - Incremental Static Regeneration (ISR) stale-while-revalidate integration in
    the frontend cache flow.
  - i18n locale detection + localized home rendering (Accept-Language +
    locale-prefixed route).
  - WebSocket ergonomic layer service integration (`/api/ws/chat`,
    `/api/ws/publish`).
- Comprehensive publish-ready documentation suite:
  - `docs/security.md` — security architecture, secret management, threat model.
  - `docs/database.md` — database architecture, multi-driver support, migration
    governance.
  - `docs/deployment.md` — deployment guide for Docker, Kubernetes, self-hosted.
  - Rewritten `README.md` with full architecture, feature, and configuration
    reference.

### Changed

- Load-test thresholds and trend artifacts expanded to service-level tracking.
- `config` crate upgraded from `0.13` to `0.14` in `krab_core` and
  `krab_orchestrator` (eliminates the `yaml-rust` unmaintained advisory).
- `DATABASE_URL` now supports `DATABASE_URL_FILE` secret sourcing via the
  `read_env_or_file` pattern.
- `service_users` database backend changed from MySQL to SQLite as the
  alternative to PostgreSQL.
- `sqlx` configured with `default-features = false` to minimize dependency
  surface.

### Removed

- MySQL database driver and all associated scaffolding code
  (`MySqlUserRepository`, `MySqlPool`, MySQL schema bootstrap).
- `rsa` crate entirely removed from the dependency tree (was pulled in
  transitively by `sqlx-mysql`).
- `yaml-rust` crate removed from the dependency tree (was pulled in by `config`
  0.13).
- `RUSTSEC-2023-0071` removed from the `deny.toml` ignore list (vulnerability no
  longer present).
- `RUSTSEC-2024-0320` removed from the `deny.toml` ignore list (vulnerability no
  longer present).
- `deny.toml` `ignore` array emptied — zero advisory exceptions.

### Fixed

- `deny.toml` syntax errors corrected for `cargo-deny` compatibility (`unsound`,
  `yanked`, `unmaintained` values).
- Deprecated `copyleft` key removed from the `deny.toml` `[licenses]` section.

### Security

- `sqlx` moved to `0.8.x` in workspace services.
- Non-local auth startup now rejects insecure/default JWT/bootstrap credentials.
- Production secret sourcing enforced via the `*_FILE` / `*_VAULT_REF` pattern.
- `cargo deny --all-features check advisories licenses bans` passes with zero
  ignores.
- All RUSTSEC advisories resolved at the crate level (not suppressed).
