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

Nothing yet.

---

## [0.2.0] — unreleased, prepared

Covers all work merged after `0.1.1` (2026-03-11).

> **Why `0.2.0` and not `0.1.2`.** This release contains breaking changes:
> `IsrCache` became async, `DistributedStore` gained required methods,
> `IsrEntry::generated_at` changed type, `ProtocolKind::parse("grpc")` stopped
> resolving, the credential format changed, and `krab_server` was removed. Under
> Cargo's semver rules a pre-`1.0` crate uses the **minor** field as its
> compatibility boundary — `0.1` and `0.2` are incompatible, `0.1.1` and `0.1.2`
> are not. Shipping this as a patch would break every downstream `^0.1` build
> without a version bump to signal it.
>
> The previous cutoff line pinned a commit (`bac72c5`) and went stale twice as
> work landed on top of it, silently: the entries below were current while the
> header claimed they could not exist.

### Security

- **Login passwords are verified as Argon2id hashes, not compared as plaintext.**
  `service_auth` resolved an expected password from `KRAB_AUTH_LOGIN_USERS_JSON`
  — a plaintext `username -> password` JSON map — and compared it with `!=`.
  That stored the secret in the clear and short-circuited on the first differing
  byte. Verification now runs through the new
  `krab_core::credentials::CredentialStore` trait against a stored PHC-format
  Argon2id hash (RFC 9106 defaults: `m=19456, t=2, p=1`), and an unknown
  username is verified against a fixed dummy hash so it costs the same as a
  wrong password rather than enumerating valid users by response time.

  **Breaking, operator-facing.** `KRAB_AUTH_LOGIN_USERS_JSON` and
  `KRAB_AUTH_BOOTSTRAP_PASSWORD` now hold Argon2id PHC hashes. Outside
  `dev`/`local`, startup rejects any other value — including one sourced
  correctly through `*_FILE` or `*_VAULT_REF`, because correct sourcing of a
  plaintext secret is still a plaintext secret. `dev`/`local` accept a plaintext
  value and hash it once at startup, so local development is unaffected. No
  deprecation window is offered; see
  [`docs/guides/migration_guide.md`](docs/guides/migration_guide.md).

- **The ISR cache was process-local, and therefore wrong under replicas.**
  `IsrCache` stored pages in an `Arc<RwLock<HashMap<..>>>` while
  `krab_core::store::DistributedStore` — with a working `RedisStore`
  implementation — sat unused beside it. Running more than one instance meant
  each kept its own divergent copy, and `invalidate_prefix` cleared exactly one
  of them, so a client refreshing a page got old or new content depending on
  which replica answered. `IsrCache` is now generic over `DistributedStore`, and
  `service_frontend` builds it from the same env-configured store the
  distributed cache already used, so setting `KRAB_REDIS_URL` is enough.

  **Breaking.** Every `IsrCache` method is now `async` and returns
  `anyhow::Result`, because a shared store can fail where a `HashMap` could not.
  `IsrEntry::generated_at` changed from `Instant` to `SystemTime` — an `Instant`
  is only meaningful in the process that created it and cannot survive a round
  trip through a store. `IsrCache::new()` still gives per-process behaviour and
  is documented as single-replica only; use `IsrCache::with_store` otherwise.

- **`DistributedStore` gained `delete` and `keys_with_prefix`.** Invalidation
  could not be expressed without them, which is why ISR had its own map in the
  first place. **Breaking** for anyone who implemented the trait. The Redis
  implementation uses `SCAN`, never `KEYS`, and escapes glob metacharacters in
  the prefix so a path containing `*` or `[` cannot over-invalidate.

- **`RedisStore` expired entries that asked not to expire.** `set` routed a
  `Duration::ZERO` TTL through `SETEX` with `.max(1)`, so a caller requesting a
  permanent entry got one that vanished after a second — which is exactly what
  an ISR `Static` policy asks for. Zero now means `SET` with no expiry, and
  `expire(.., ZERO)` issues `PERSIST` rather than an `EXPIRE 0` that would
  delete the key.

- **Generated split-topology projects shipped a test that could not fail.**
  `krab topology split` emitted an assertion that a literal array contained a
  literal it had just been built from. It reported green forever under a name
  claiming local-vs-remote contract coverage — worse than no test, because it
  answered the question before anyone asked it. It is now `#[ignore]`d with a
  reason, panics if run anyway, and carries a worked example of what real
  coverage looks like. Two tests in `krab_cli` guard against the tautology
  returning.

### Removed

- **`krab_server` is deleted.** 502 lines that never handled a request: the
  workspace contained zero `use krab_server` sites, and
  `service_frontend/build.rs` has always generated `axum::Router` registration.
  It also had no HTTP method routing — a `GET` and a `POST` to one path were
  indistinguishable — and a trie that did not backtrack, so a request for `/a/c`
  returned 404 against a registered `/:x/c`. Axum is now the stated SSR
  foundation, which is what it had always been in practice. Its static-path
  traversal defence was ported to `krab_core::static_assets` (behind `rest`)
  **before** the deletion, with its original tests plus four new cases, and both
  routing defects are now pinned by regression tests in `service_frontend`. See
  [ADR 0005](docs/adr/0005-krab-server-disposition.md). The crate was never
  published, so no consumer is affected.

### Changed

- **The `grpc` feature and `krab_core::grpc` are renamed to `grpc-semantics`
  and `krab_core::grpc_semantics`.** The feature enabled zero dependencies and
  the module is 159 lines of status codes and `grpc-timeout` header parsing —
  gateway vocabulary, not a transport. `tonic` and `prost` appear nowhere in the
  workspace. The old names are kept as deprecated aliases for one minor version
  and are removable no earlier than `0.3.0`. See
  [ADR 0007](docs/adr/0007-grpc-feature-disposition.md).
- **`ProtocolKind::parse("grpc")` now returns `None` instead of
  `Some(ProtocolKind::Rpc)`.** A service configured with
  `KRAB_PROTOCOL_ENABLED=grpc` previously started, exposed Krab's
  JSON-over-HTTP RPC, and reported itself as satisfying a gRPC requirement it
  cannot satisfy. Configuration validation now fails instead. **Breaking** for
  anyone using that spelling; use `rpc`.

### Fixed

- **The client half of `#[server]` had never compiled.**
  `krab_core::server_fn` was gated behind `rest`, a server-only feature that
  pulls axum, so `call_server_fn` — which the macro's wasm32 stub calls — did
  not exist in a browser build. The module's own internals already branched on
  `not(feature = "rest")` and `target_arch = "wasm32"`, so it was written to work
  without `rest`; the module declaration made that code unreachable. It is now
  available under `rest` **or** `web`. Lifting the gate also exposed
  `ServerFnRegistration` being defined twice with `rest` off, and missing
  `wasm-bindgen-futures` and `web-sys` features for the fetch path. All fixed;
  the browser build is now compiled and linted in CI by the new
  [`reference-app`](.github/workflows/reference-app.yaml) gate.
- **`service_users` asserted a database default the framework no longer has.**
  Promoting SQLite into `krab_core` moved the `KRAB_DB_DRIVER` default from
  SQLite to Postgres — deliberately, since a production service silently
  falling back to a local SQLite file is worse than one that refuses to start —
  but two `service_users` tests still asserted the old default, leaving
  `cargo test --workspace` red on `main`. The tests now assert the framework
  default and set the driver explicitly where they need SQLite.

### Added

- **A client-side router** in `krab_client::router`. Krab renders on the server
  and hydrates islands, but every in-app link was a full document request, which
  discarded hydrated island state, scroll position, and the warm WASM module.
  `router::start()` intercepts same-origin anchor clicks, fetches the
  destination, swaps the contents of the element marked
  `data-krab-router-outlet`, updates history, and re-hydrates.

  The interception rules are a pure function (`should_intercept`) with 16 tests,
  because this is where client routers go subtly wrong: ctrl-click, middle-click,
  `target="_blank"`, `download`, cross-origin, and already-handled events are all
  left to the browser. Every failure path — no outlet in either document, a
  non-OK response, an offline network — falls back to a normal navigation rather
  than a blank page.

  **Deliberately not included:** nested layouts, prefetch, and a client-side
  route table. The server stays authoritative for routing, so SSR, ISR, and
  render policy are not duplicated in two places. See the module docs.

- **A browser test harness for the hydration runtime**
  (`crates/framework/krab_client/tests/hydration_browser.rs`, run by the new
  `client-browser-tests` CI job via `wasm-pack test --headless --chrome`).
  `cargo test --workspace` compiles `krab_client` for the host, where there is
  no `document`, so it could only ever reach the pure planning functions — every
  DOM-mutating path had no test at all, and a hydration defect surfaces as a
  subtly wrong DOM in a user's browser rather than a red build. Seven tests
  cover node reuse, mismatch patching and counting, unregistered islands,
  malformed props not aborting sibling islands, idempotency, and removal of
  stale server-rendered children.

- **A vendored reference application** at
  [`examples/reference_apps/islands_rpc/`](examples/reference_apps/islands_rpc/):
  one page, built entirely with `view!`, rendering two `#[island]` components
  with distinct props, one of which calls a `#[server]` function from its click
  handler. `#[island]` and `#[server]` previously had **zero** usages outside
  the framework's own tests and documentation, so nothing demonstrated that SSR,
  hydration, and RPC worked together — and building the first real consumer is
  what surfaced the `server_fn` gating bug above. It is a workspace member; CI
  builds, tests, and lints it on both the native and `wasm32` targets and builds
  its WASM bundle.
- **[`docs/guides/getting_started.md`](docs/guides/getting_started.md)** —
  install, scaffold, first page, first island, first server function. The
  documentation set had 29 files and no file matching `*start*`, `*quick*`, or
  `*tutorial*`; nothing covered building your own application.
- **`krab_core` gained an `auth` feature** providing
  `credentials::CredentialStore`, `EnvHashCredentialStore`, `hash_password`,
  `verify_password`, and `is_valid_password_hash`. Off by default, so a consumer
  that issues no passwords compiles no KDF.
- **`krab auth hash-password`** generates credentials in the format the auth
  service verifies. Reads the password from stdin by default so it stays out of
  the process list and shell history; `--username <name>` emits a ready-to-paste
  single-entry JSON map. Previously the only documented credential format was
  plaintext, so there was nothing to generate.
- **The workspace is publishable.** Every inter-crate dependency now carries a
  `version` alongside its `path`, declared once in `[workspace.dependencies]`.
  Previously every framework and tooling crate was path-only and
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
- **Per-crate `README.md` for every publishable crate** — `krab_core`,
  `krab_client`, `krab_macros`, `krab_cli`, `krab_orchestrator`. (`krab_server`
  also got one; it is removed later in this same unreleased cycle, so five
  crates ship rather than six.)
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
