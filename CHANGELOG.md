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

### Added

- `KRAB_HTTP_OVERLOAD_MODE=queue|shed`: support for fast-failing excess concurrent HTTP
  requests with `503 Service Unavailable` (`SERVICE_OVERLOADED`) via `tower/load-shed`.
- Signal WASM microtask drain chain depth limiter (`DRAIN_CHAIN_DEPTH`) bounded at
  `MAX_FLUSH_DEPTH = 64`, cutting off runaway self-writing and mutually recursive signal
  loops. It counts consecutive drain generations that keep queueing more effects — the
  browser analogue of the native `FLUSH_DEPTH` frame count — so a wide fan-out of
  independent effect writes in one flush is delivered in full.
- `ErrorCategory::Unavailable` (HTTP 503), distinct from `RateLimited` (429): the service
  is out of capacity rather than the caller being over quota.
- `krab_client` headless browser integration suite `cycle_browser.rs` covering WASM signal self-writes,
  infinite loop cutoff, wide effect fan-out delivery, and scoped effect disposal.
- `krab new <name> --template fullstack` scaffolds a complete full-stack Krab
  application with server-side rendering (SSR), client-side WASM island hydration
  via `krab_client`, `#[server]` functions, and static file serving (`/pkg` and
  `/public` via `tower-http`) out of the box. Dual-target dependencies (native
  axum/tokio/rest and wasm32 web/hydration) and wasm-pack build configurations
  are generated automatically.

### Changed

- **Breaking:** `krab_core::http::ErrorCategory` is now `#[non_exhaustive]`. Rust callers
  that `match` on a category need a wildcard arm. Taken in the same release as the
  `Unavailable` addition so that every future category is a non-breaking addition;
  constructing the existing variants is unaffected.
- `KRAB_HTTP_OVERLOAD_MODE` is trimmed and validated. An unrecognised value falls back to
  `queue` and logs `env_value_invalid_using_default` instead of selecting it silently.

### Governance

- `krab db rehearsal` now requires a reachable database (`KRAB_REQUIRE_DB_TESTS=1`) and
  writes no evidence file when the rehearsal does not run. It previously recorded
  `rollback_rehearsal: ok` when the underlying test skipped for want of a database —
  an artifact RELEASE_POLICY trusts as proof of rehearsal, produced by a rehearsal that
  never happened.
- `db-lifecycle.yaml` runs `krab_core`'s `db_tests` against its Postgres service. Ten of
  those eleven tests — migration ledger, checksum rewriting, drift policy, governance
  gating, promotion rules — previously ran against a real database nowhere in CI; only
  the rollback rehearsal did. `ops-hardening.yaml`'s per-driver steps are documented as
  compile-isolation checks, which is all they ever were.
- `db-lifecycle.yaml` sets `KRAB_REQUIRE_DB_TESTS=1`. The job provides Postgres, so a skip
  there means the service container broke; the gate no longer degrades silently to green.
- `[workspace.metadata.krab] next_version` declares the version `[Unreleased]` will ship
  as, and `scripts/check_workspace_layout.py` fails any `#[deprecated(since = ..)]` or
  `docs/reference/api.md` "As of X" reference naming a release beyond it. Those references
  have to name a release before it exists, so a renumbered release used to leave them
  silently false — and a `since` naming the wrong version is worse than none.

### Security

- `h2` bumped to 0.4.16 for [RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258)
  (unbounded queueing of empty DATA frames in the `hyper` stack). Lockfile only.

### Deprecated

- `krab_core::db::postgres::run_migrations`, superseded by `run_versioned_migrations`.
  It only creates an unused `_krab_migrations` table. Removed in 0.6.0.

### Fixed

- `KRAB_HTTP_OVERLOAD_MODE=shed` now answers `503 Service Unavailable` as documented; it
  was returning `429 Too Many Requests`, which load balancer and alert policies keyed on
  503 would not match.
- `service_frontend` ISR cache keys escape `@` and `%` in the path and locale, so a request
  path that itself contains `@` can no longer be read as a locale suffix — the multi-locale
  poisoning the locale dimension was added to prevent. Existing cached entries miss once
  under the new key format.
- `krab new --template fullstack` output passes `cargo fmt --all --check`: the crate's own
  `use` line is now emitted in sorted position rather than at a fixed offset.
- `krab new --template fullstack` takes `krab_client` with `default-features = false`, so
  the deprecated `demo-islands` `Counter` no longer collides with the template's own
  `Counter` in the island registry.
- `krab dev` builds the client with `cargo build --lib`, so a single-crate project with both
  a `[[bin]]` and a `cdylib` no longer tries to compile its axum/tokio binary for
  `wasm32-unknown-unknown`, and it looks for the wasm artifact under the crate name cargo
  actually emits (`demo_fullstack.wasm`, not `demo-fullstack.wasm`).
- `service_auth` token revocation and refresh marker persistence now fail closed with
  `503 Service Unavailable` on store errors instead of silently swallowing failures.
- `service_frontend` ISR cache keying and revalidation now incorporate the locale dimension,
  preventing multi-locale cache poisoning and English revalidation overwrites.
- `service_frontend` request-path `spawn_blocking` task failures map to 500 error responses
  instead of panicking worker threads.
- `KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN` routed through `krab_core::config::read_env_or_file`.

### Removed

- Removed unused `SignalId` from `krab_core::signal`.

---

## [0.4.0] — 2026-08-12

The release that makes the published crates match the documented framework.
Three audits land together: `krab_client` ships the real hydration runtime
instead of the inert stub that 0.1 and 0.2 put on crates.io, `krab_macros` and
`krab_orchestrator` get their first dedicated hardening pass, and `krab_cli`
stops failing in the projects it scaffolds. It contains breaking changes (see
**Changed**), so it takes the minor field per Cargo's pre-`1.0` semver rules.

> `0.3.0` was tagged and released on GitHub but never published to crates.io.
> The registry therefore moves `0.2.0` → `0.4.0`, and `0.4.0` carries the
> `0.3.0` changes as well. Consumers upgrading from `0.2.0` should read both
> this section and [`[0.3.0]`](#030--2026-08-12) — in particular the protocol
> resolution and ISR cache changes, which are breaking.

### Added

- `krab_client` can hydrate and release a subtree rather than only the whole
  document: `hydrate_within(root)` walks one element, and `unmount(root)`
  detaches the listeners, dynamic regions, and effects hydration created under
  it. Markup inserted after first paint — a modal, a lazily fetched panel — can
  now be brought to life and torn down again without leaking into the page for
  its remaining lifetime.
- Client-side routing is reachable from the browser bundle as `start_router()`.
  Same-origin link clicks and Back/Forward replace the contents of the element
  marked `data-krab-router-outlet` and re-hydrate, instead of reloading the
  document and discarding hydrated island state, scroll position, and the warm
  WASM module. Modified and non-primary clicks, `target`, `download`,
  cross-origin URLs, and an explicit `data-krab-router-ignore` are left to the
  browser, and every failure — no outlet on either page, a failed fetch — falls
  back to a full navigation. The router's logic shipped in 0.2.0 but was
  unreachable: it sat behind a feature no documented build enabled.
- Router navigations send an `x-krab-router: 1` request header, so a server can
  distinguish an in-app navigation from a document request — for logging, or to
  serve a lighter shell. Documented in
  [`docs/reference/api.md`](docs/reference/api.md).
- `krab completions <bash|zsh|fish|powershell|elvish>` writes a shell
  completion script for the `krab` binary to stdout.
- `--json` reaches the CI-facing commands that previously had no machine-readable
  output: `krab doctor` (per-check name, level, details, plus the counts and the
  `success` verdict computed under the same `--strict` rule as the exit code),
  `krab topology doctor` (violations and skipped checks), `krab env-check`, and a
  minimal `{command, status, error}` envelope for the `contract`, `db`, and
  `security` gates. `--json` changes only what is printed, never the exit status.
- `krab doctor` distinguishes a check that did not apply from one that passed.
  Checks scoped to a Krab framework checkout report `SKIP` with the reason, and
  skips are never fatal — including under `--strict`, because "not applicable" is
  a fact about the project, not a defect.
- Rustdoc for `#[island]` and `view!`, which were public API with none.
- `#[server]` expansion tests. The macro previously had only compile-fail
  coverage and a doctest, so nothing exercised the generated handler, the
  dispatch shim, the args struct, or the `ServerFn` marker.
- Tests for `resolve_startup_order`, which had none: dependency ordering,
  determinism, cycles, self-cycles, and unknown dependencies.
- Compile-fail cases for an unclosed element, an unclosed fragment, a generic
  `#[server]` function, and `#[island]` with an argument.

### Changed

- `--diagnostics` and `--json` are global flags rather than per-subcommand ones
  (they were redeclared on ten subcommands, and `--json` existed only on `release
  check`/`certify`). Every existing invocation keeps working: clap accepts a
  global flag before or after the subcommand.
- `krab release certify --out` defaults to `internal/audit/release-certify/local`
  instead of `release-evidence`, which created an untracked directory in the
  repository root that no `.gitignore` rule covered. The certification index is
  now written relative to `--out` rather than to a hardcoded
  `internal/audit/release-certify/`, so running the command in a consumer's
  project no longer creates a framework-specific tree inside it. CI is unaffected:
  it passes `--out` explicitly.
- The release evidence bundle creates only the four sections that receive
  artifacts. `02-security`, `05-performance`, `06-observability` and
  `07-deployment-rehearsal` were created empty on every run, reading as coverage
  that did not exist.
- `#[server]`'s dispatch shim delegates to the generated handler instead of
  repeating it. Argument decoding and response mapping were written out once per
  (handler, dispatch) × (stream, non-stream) — four copies of the same logic.
  No behaviour change; the shim's signature and the `ServerFn` contract are
  unchanged.
- New optional `restart_policy.stability_window_ms` per service in `krab.toml`,
  defaulting to 60000.
- `krab_orchestrator` sets `kill_on_drop` on spawned services, so a panicking or
  killed orchestrator no longer leaves every service running with its port bound.

### Deprecated

- `krab_client`'s bundled demo islands — `Counter`, `Toggle`, `Likes` and their
  props types — are deprecated and removed in 0.6.0, together with the new
  `demo-islands` feature that now gates them (on by default until then). Each
  `#[island]` is `inventory::submit`ed under its plain function name and
  `inventory` links every submission in the final binary into one registry, so
  these three names are claimed in every consumer's bundle. An application that
  defines its own `Counter` island gets two entries under one name and
  `hydrate()` resolves them with `find()` — first match wins, decided by link
  order, with no diagnostic. Define islands with `#[island]` in your own crate;
  `default-features = false` removes them today.
### Fixed

- `krab gen route|component|server-function` wires its output into the generated
  project's module tree. It previously wrote a source file that nothing
  declared, so the code was never compiled and the route silently did not
  exist — `krab new my_app && krab gen route dashboard && cargo run` produced no
  `/dashboard`. The route generator's contract was documented as
  `service_frontend/build.rs` auto-discovery, which exists only inside the
  framework workspace and never in a scaffolded project. `krab new` now emits
  `// krab:modules` and `// krab:routes` markers, and `krab gen` inserts module
  declarations and router registrations at them. Re-running is idempotent and
  never overwrites a file you have edited; when the markers are absent the
  command prints the wiring to add by hand rather than rewriting a `main.rs` it
  did not generate.
- Routes added by `krab gen route` to a `saas` project no longer bypass the
  common HTTP layers. The merge now happens while the router still carries
  `AppState`, so generated routes get the same telemetry, timeouts, and
  middleware as the hand-written ones instead of silently skipping them.
- `krab new` no longer creates empty `src/routes`, `src/api`, and `docs`
  directories. Git does not track empty directories, so they vanished for anyone
  who cloned a scaffolded project — and the generated Dockerfile's
  `COPY public/ public/` then failed the container build. `public/` is pinned
  with a `.gitkeep`; the directories the scaffolder never populated are gone.
- `krab new` validates the project name before touching the filesystem, against
  Cargo package rules, Rust keywords, and Windows device names, and suggests a
  valid slug when it rejects one. `krab new "My App"` previously wrote an
  unbuildable project whose failure surfaced only at `cargo run`.
- Generated `.env.example` no longer sets `KRAB_SECRETS_SOURCE`, a variable no
  Krab code reads and no reference page documents, and no longer ships
  `change-me-in-production` as an inline secret for a configuration that
  `staging` and `prod` reject at startup. It documents the
  `NAME` → `NAME_FILE` → `NAME_VAULT_REF` resolution order and shows the `_FILE`
  promotion path beside each secret. A test now parses the generated file and
  asserts every variable against
  [`docs/reference/environment.md`](docs/reference/environment.md).
- The generated CI workflow no longer contains steps that cannot fail. The
  `saas` template's `cargo test -- db_` step matched no tests and pointed
  `DATABASE_URL` at a Postgres the workflow never started; it is removed rather
  than made real, which would have left a starter whose `cargo test` fails on
  any machine without a database. Every template now scaffolds a `/health` smoke
  test, so `cargo test` in a new project asserts something.
- The generated Dockerfile floats to `rust:1-slim-bookworm` and the generated
  manifest declares `rust-version`. The image previously pinned `rust:1.77`
  while the manifest floated its dependencies, so a dependency raising its MSRV
  broke the container build with an error that looked nothing like the cause.
- `krab new` runs `git init` in the project it scaffolds, with `--no-git` to opt
  out. It wrote a `.gitignore` for a repository it never created. Initialisation
  is skipped inside an existing work tree, no commit is created, and a missing
  or failing `git` is a warning rather than a failed scaffold.
- The `edge-ssr` template emits a real `docs/render_policy.md` describing the
  render mode, ISR cache policy, and edge eligibility it actually configures.
  The reference app READMEs had directed users to that file since they were
  written; it had never been generated.
- The WASM bundle contains the hydration runtime. `krab_client`'s `web`
  feature — which gates `hydrate()`, the reconciler, and the router's browser
  half — had no default, and the three build paths that produce the bundle (the
  WASM size gate, the documented `wasm-pack` command, and `krab build
  --release`) all omitted it. Each produced a ~15 KB artifact whose `hydrate()`
  logged one line and returned, in place of the ~198 KB runtime; pages loading
  it saw no error and quietly ran their JavaScript fallback instead. `web` is
  now a default feature, all three paths name it explicitly, and the size gate
  fails if the artifact does not contain the runtime — size alone could not tell
  a stub from a build, since the stub passed every threshold with room to spare.
- `hydrate()` is idempotent. Calling it twice — which client-side navigation now
  does on every page swap — previously re-registered every island's event
  listeners, so a single click fired its handler once per call.
- Hydration no longer retains DOM state for the life of the page. Event
  closures, dynamic-region records, and the effects created for reactive regions
  were kept alive after the nodes they belonged to were gone, so every
  re-render, list update, and navigation added to a set that was never reduced.
- Back and Forward work on any URL carrying a fragment. The router's
  same-page-fragment guard compared the browser's current URL against itself —
  `popstate` fires *after* the address bar has already moved — so every history
  navigation to a URL containing `#` was classified as an in-page hash change
  and silently dropped: no refetch, no content swap, and no error. The guard now
  compares the destination against the URL the outlet's content actually came
  from.
- The router's outlet scanner no longer mistakes markup inside comments,
  `<script>`/`<style>` bodies, or attribute values for real tags. A page whose
  script mentioned `data-krab-router-outlet` could select the wrong element, and
  a `</main>` inside a comment or script *within* the outlet truncated the
  swapped content. Document titles carried across a navigation are now
  entity-decoded and tolerate attributes on `<title>`.
- **`krab doctor` works outside the framework workspace.** It read
  `crates/framework/krab_core/src/service_contract.rs` unconditionally, so in any
  project scaffolded by `krab new` it printed `Error: Failed reading …` and exited
  1 — and because one evaluator's error aborted the whole run, the three checks
  that had already succeeded were discarded. The generated README tells users to
  run exactly that command. Framework-only paths are now applicability checks,
  and a failing evaluator becomes a `FAIL` entry in a report that still prints
  every other check.
- **`krab doctor --strict` passes on an untouched scaffold.** A generated
  `krab.toml` declares `[project]` and no `[services.*]` — one binary, nothing
  for the orchestrator to supervise — and the service-config check warned about
  it, which `--strict` promoted to a failure. A brand-new project failed its own
  gate before its author had written a line.
- **`krab gen component|route|server-function` no longer destroys hand-written
  files.** They wrote with no existence check, so re-running one silently
  replaced an edited file with boilerplate while printing "created successfully".
  An existing file is now kept, and the run reports that nothing was overwritten.
- **Generated components and routes are part of the build.** The scaffold
  declared no modules for them and the generator printed no instruction to add
  any, so every file `krab gen` produced was dead code that `cargo build` never
  saw. The scaffold now carries wiring markers and `krab gen` registers what it
  generates.
- `krab gen service --type grpc` requests `krab_core`'s canonical
  `grpc-semantics` feature, and the `krab db *` gates request `db-postgres`.
  Both previously named the deprecated `grpc`/`db` aliases, which are scheduled
  for removal — generated projects were being pinned to a feature due to be
  deleted. `--type rpc` now says that it enables the `rest` feature, rather than
  leaving the user to discover it.
- **The release evidence bundle contains evidence.** Steps ran with inherited
  stdio and captured nothing, so `01-test-and-lint/clippy.txt` held three lines
  restating the summary instead of the clippy output the bundle exists to
  preserve. Command output is now captured into the artifact, and a failing
  step's transcript is echoed so a local run stays debuggable.
- Certification timestamps are RFC 3339 UTC rather than bare Unix epoch seconds
  rendered as `timestamp: 1786…`.
- The `secure_headers`, `csrf_strategy` and `telemetry_initialization` release
  gates no longer pass on a mention inside a comment or a test file. They were
  plain substring greps over every `.rs` file; one was being satisfied partly by
  prose in a doc comment.
- Fingerprinted asset filenames are derived from SHA-256 rather than
  `DefaultHasher`, whose output is documented as unstable across Rust releases —
  a toolchain bump silently renamed every asset and busted downstream caches.
  This is the same reasoning `krab_core` already applies to migration checksums.
- `krab dev --watch` no longer orphans its frontend process. Several error paths
  returned while the spawned child was still alive, leaving a `cargo run` holding
  the port so the next `krab dev` failed to bind.
- `krab auth hash-password` prompts on stderr when stdin is a terminal. With no
  `--password` it blocked on stdin with no output at all, which looks like a
  hang. Piped input is unchanged.
- The `content_site` reference-app README no longer tells readers to run `krab
  release certify` before publishing — a governance command hardcoded to the
  framework workspace's own services. The same correction was made for the `krab
  new` README in 0.3.0 and missed this one.
- **`krab_orchestrator` exits non-zero when `krab.toml` is missing or
  malformed.** The load error was logged and then discarded, so a failed start
  returned exit code 0 — CI, `krab bootstrap`, and any process supervisor above
  the orchestrator all read it as a clean run.
- **A service that exits and is not restarted no longer floods the log.** Its
  handle stayed in the supervised-children map after being reaped, and Tokio
  caches a reaped child's exit status, so every 500 ms tick re-reported the same
  exit: `service_exited` and `service_restart_limit_reached` repeated twice a
  second, indefinitely. Reaped handles are now removed.
- **The restart budget is no longer permanent.** `max_attempts` counted every
  restart for the lifetime of the orchestrator, so a service that crashed once a
  day was permanently dead after `max_attempts` days. A service that stays up for
  a stability window (`restart_policy.stability_window_ms`, default 60 s) now
  gets its budget back.
- **A crashed service no longer stalls supervision of every other service.** The
  restart backoff was an inline `sleep` inside the supervision loop, which also
  made Ctrl-C unresponsive for its duration. Restarts are now scheduled against a
  deadline and the loop keeps ticking.
- **A transient filesystem-scan error no longer kills the orchestrator and
  orphans its children.** `watch_fingerprint` propagated `read_dir` errors out of
  the supervisor without a shutdown pass, so a file removed mid-scan by a
  concurrent build left every service running with no supervisor. Per-entry
  errors are logged and skipped, and a failed scan skips the cycle.
- **Watch restarts follow dependency order.** They iterated `config.services`, a
  `HashMap`, so the frontend could come back before the auth service it depends
  on. Services now stop in reverse dependency order and start in forward order,
  matching initial startup. Shutdown is likewise ordered.
- **`watch.poll_ms = 0` no longer spins.** It slept for zero milliseconds and
  rescanned every watched source tree, pinning a core. Both `poll_ms` and
  `settle_ms` are floored at 50 ms.
- **A readiness probe stops as soon as the child exits.** A service that died on
  startup — bad port binding, failed migration, config panic — burned its entire
  retry budget probing a dead process and then reported a generic connection
  error. The failure now names the exit status.
- **`view!` reports an unclosed tag as an unclosed tag.** `view! { <div>"hi" }`
  reported `view! macro body is empty`, pointing at the whole macro, because the
  exhausted token stream reached the node parser's empty-input check.
- **`#[island]` props no longer have to be `Clone`.** The server half rendered
  `inner(props.clone())` although the serialization above it only borrowed.
- **`#[island]` rejects arguments instead of ignoring them.** The attribute
  token stream was discarded, so `#[island(lazy)]` — or any misspelt option —
  compiled and silently did nothing.
- **A props value that fails to serialize is marked as such.** `#[island]` used
  `unwrap_or_default()`, emitting `data-props=""`, which the browser then
  reported as a *client* decode failure. The boundary now carries
  `data-krab-boundary-state="props-encode-error"`, distinct from a client-side
  decode error. The serde message is deliberately not placed in the markup.
- **`#[server]` rejects generic functions and `where` clauses.** The expansion
  rebuilds the function from its signature pieces and never re-emitted
  `sig.generics`, so a generic server function expanded into a body referencing
  undeclared type parameters and failed with `cannot find type` against generated
  code.

### Governance

- `krab_cli` has an integration suite. It had ~88 unit tests and no `tests/`
  directory, and every one of them passed while `krab doctor` was unusable in
  every generated project — because each called an evaluator directly with a
  hand-built root. The new tests run the real binary in a real `krab new`
  project, covering the `new` → `doctor` round trip, generator idempotence and
  no-clobber, generated-module wiring, and the deprecated-alias bans.

---

## [0.3.0] — 2026-08-12

A robustness and hardening release from a full `krab_core` runtime audit,
landed in two tranches. It contains breaking changes (see **Changed**), so it
takes the minor field per Cargo's pre-`1.0` semver rules.

### Security

- Protocol resolution now runs **after** authentication. Tenant protocol policy
  (`KRAB_PROTOCOL_TENANT_OVERRIDES_JSON`) is selected from the authenticated
  tenant claim; the client-supplied `x-krab-tenant-id` header / `?tenant_id=`
  query fallback is disabled unless `KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED=true`
  (dev only, logs on use). Previously a client could spoof or omit the header to
  inherit another tenant's overrides or escape its own. See ADR 0010.
- Requests to a route family whose protocol is disabled are rejected with 400
  `PROTOCOL_NOT_SUPPORTED` instead of passing through to the handler.
- With `KRAB_TRUST_PROXY_HEADERS=true`, the client IP is taken from the rightmost
  trusted `X-Forwarded-For` entry (`KRAB_TRUSTED_PROXY_HOPS`, default 1) and must
  parse as an IP; the leftmost, client-controlled entry could previously spoof
  rate-limit identity.
- `RuntimeState::try_new()` fails startup in staging/prod/unknown environments
  when `KRAB_REDIS_URL` is set but Redis cannot be used — whether the URL is
  broken or the binary was compiled without the `redis-store` feature — instead
  of silently downgrading to a per-process store (which breaks shared rate
  limiting and token revocation). Dev warns and falls back. All in-repo services
  boot through it.
- Server-function errors converted from `anyhow::Error` no longer serialize the
  underlying error chain (connection strings, SQL fragments, filesystem paths)
  into the client-visible 500 envelope. The full chain goes to the server log
  (`server_fn_internal_error`); the wire message is a generic
  `internal server error`.
- Malformed `KRAB_AUTH_ROUTE_POLICIES_JSON` now fails closed (500 +
  `auth_route_policies_json_malformed_failing_closed`) instead of silently
  dropping every route restriction. New `try_load_route_policies()` exposes the
  parse result.
- Malformed `KRAB_PROTOCOL_RESTRICTED_OPS_JSON` / `KRAB_PROTOCOL_TENANT_OVERRIDES_JSON`
  no longer silently drop every protocol restriction: `ProtocolConfig::validate()`
  reports bad JSON and unknown protocol names as startup errors.
- `DbConfig`'s `Debug` output redacts the database-URL userinfo, so a `{:?}` can
  no longer print the `DATABASE_URL` password.

### Added

- `ErrorCategory::RateLimited` (429, wire `rate_limited`) and
  `ErrorCategory::Unauthenticated` (401, wire `unauthenticated`).
- Request timeout (`KRAB_HTTP_REQUEST_TIMEOUT_SECS`, default 30 s → 408) and
  concurrency limit (`KRAB_HTTP_MAX_CONCURRENCY`, default 1024) layers in the
  common HTTP stack.
- `JwtVerifierCache` on `RuntimeState`: JWT providers and decoding keys are
  parsed once per state instead of per request; `authorize_with_jwt_cached`
  exposed, `authorize_with_jwt` retained as an env-building wrapper.
- `DistributedStore::set_if_absent` (atomic create-if-missing; Redis
  `SET NX PX`), and `IsrCache::serve_or_lease` / `release_lease` /
  `IsrServeOutcome` for cold-miss single-flight rendering. See ADR 0011.
- `MemoryStore::with_max_entries` and `KRAB_MEMORY_STORE_MAX_ENTRIES` capacity
  cap (default 100 000, 0 = unlimited) with throttled `memory_store_evicted`
  telemetry.
- `WsRoom::join() -> WsConnectionGuard` (RAII connection tracking),
  `WsRoomManager::reap_empty()` / `try_room()` / `with_max_rooms()`, and an
  optional room cap via `KRAB_WS_MAX_ROOMS`.
- `CircuitBreakerConfig` with `TripPolicy::FailureRate` (rolling-window) mode,
  `SharedCircuitBreaker` for cross-task sharing, and a full circuit-breaker
  unit-test suite (previously zero tests).
- `signal::create_effect_scoped` returning `EffectHandle` — the first disposal
  API for root effects.
- `TopologyRuntime::from_env_checked()` — errors on unrecognized topology,
  unparseable endpoints JSON, and distributed mode with an empty endpoint map;
  `krab topology doctor` and `krab release check` run it.
- `FRONTEND_ISR_QUERY_ALLOWLIST` bounds which query params enter `service_frontend`
  cache keys (default path-only).

### Changed

- **Breaking:** `ChunkedStreamWriter::finish()` returns `FinishedStream` (chunks
  plus `budget_exceeded`/`cancelled` flags) instead of `Vec<String>`; `write()`,
  `write_suspense_marker()`, and `render_to_chunk_stream()` return `bool`
  (accepted/dropped), the first two `#[must_use]`.
- **Breaking:** `DistributedStore` gained the required method `set_if_absent`;
  external implementors must add it.
- **Breaking:** `WsRoom::connections()` is synchronous (atomic counter); drop the
  `.await`.
- **Breaking (wire):** an `ApiError` with `category=authz` / `code=UNAUTHORIZED`
  now maps to 403 (was 401) — use `Unauthenticated`; `code=TOO_MANY_REQUESTS`
  under other categories no longer forces 429 — use `RateLimited`. The error
  envelope `category` gains `rate_limited` and `unauthenticated`.
- **One-time ISR cache flush:** the ISR key separator changed from `:` to a
  control byte so a namespace cannot prefix-match a sibling. Old-format entries
  miss once and repopulate (the single-flight lease absorbs the burst);
  no-TTL `Static` entries linger unread in Redis — see the migration guide for
  cleanup. `service_frontend` cache keys are now normalized (path + allowlisted
  query params).
- `CircuitBreaker` counts the cooldown-expiry request as the first half-open
  probe (previously `half_open_max_probes + 1` requests could pass).

### Deprecated

- `KrabConfig::from_env` (panics on invalid `KRAB_PORT`) — use
  `from_env_checked`. All in-repo callers migrated.
- `WsRoom::connect()` / `disconnect()` — use `join()`'s RAII guard.

### Fixed

- `run_versioned_migrations` serializes concurrent migrators with a session-level
  Postgres advisory lock; a rolling deploy could otherwise crash a pod on the
  `krab_migrations` primary key. Migration/rollback bodies now execute via
  `sqlx::raw_sql`, so multi-statement migrations work.
- `DbConfig::from_env` rejects unparseable pool values and validates
  `1 <= min <= max` with non-zero timeouts; `connect_with_config` fails fast on
  auth/config/TLS errors instead of exhausting the retry schedule;
  `enforce_promotion_policy` rejects unknown environment names instead of
  treating them as `local`; `enforce_migration_governance` records the audit row
  then denies (`Err`) when `DB_MIGRATION_ALLOW_APPLY=false`.
- `enforce_migration_governance` creates the rollback-rehearsal ledger before
  querying it, so a fresh release DB reports the governance verdict, not
  `relation does not exist`.
- An effect that writes its own dependency no longer overflows the stack: the
  re-entrant run is refused (`signal_effect_cycle_detected`), delivered after the
  body finishes so it still converges, and capped (`signal_flush_depth_exceeded`).
  A self-referential memo serves the stale value instead of panicking. Interleaved
  parent/child reads no longer grow a signal's subscriber list. `ErrorBoundary`
  fallbacks that panic degrade to a minimal error div. On wasm, an `Action`/`Resource`
  whose future panics no longer latches `pending` forever.
- Render-budget exhaustion is no longer silent: `write()` returns `false`,
  `finish()` reports `budget_exceeded`, and the first drop warns
  `render_budget_exceeded`; dropped suspense markers no longer inflate telemetry.
- WebSocket connection counts no longer leak on a panicked/aborted task, and
  `service_frontend`'s chat socket no longer double-decrements the count.
- `MemoryStore` reaps expired entries during writes; the `krab_inflight_requests`
  gauge decrements via a drop guard; `serve_with_graceful_shutdown` handles
  SIGTERM on Unix; `init_tracing` uses `try_init` instead of panicking on
  double-init; native `call_server_fn` uses one shared timed `reqwest` client and
  truncates non-JSON error bodies to 2 KiB.
- `cors_middleware` no longer 403s an OPTIONS request that carries no `Origin`
  (non-CORS preflight reaches the router), and origin-dependent CORS responses
  carry `Vary: Origin`.
- `TopologyRuntime::from_env` warns instead of silently swallowing malformed
  `KRAB_RUNTIME_TOPOLOGY` / `KRAB_RUNTIME_ENDPOINTS_JSON`.

---

## [0.2.0] — 2026-08-12

First public release: pushed to
[github.com/ManirajKatuwal/krab-pub](https://github.com/ManirajKatuwal/krab-pub)
and published to crates.io (`krab_core`, `krab_macros`, `krab_client`,
`krab_cli`, `krab_orchestrator`). Covers all work merged after `0.1.1`
(2026-03-11).

> **Why `0.2.0` and not `0.1.2`.** This release contains breaking changes:
> `IsrCache` became async, `DistributedStore` gained required methods,
> `IsrEntry::generated_at` changed type, `ProtocolKind::parse("grpc")` stopped
> resolving, the credential format changed, and `krab_server` was removed. Under
> Cargo's semver rules a pre-`1.0` crate uses the **minor** field as its
> compatibility boundary — `0.1` and `0.2` are incompatible, `0.1.1` and `0.1.2`
> are not. Shipping this as a patch would break every downstream `^0.1` build
> without a version bump to signal it.

### Security

- **The unauthenticated ("open") path list is configurable via
  `KRAB_AUTH_OPEN_PATHS`.** It was hardcoded in `auth_middleware`, so
  operators could not close `/metrics` or `/metrics/prometheus` without
  forking the middleware. When set, the variable replaces the built-in list
  (comma-separated, trailing `*` for prefix match; an explicitly empty value
  closes everything); unset keeps the exact previous list, so defaults are
  backward-compatible. `KRAB_AUTH_PUBLIC_PATHS` still adds paths on top.
  Documented in `.env.example` and `docs/reference/environment.md`.

- **A JWT algorithm allowlist mixing HMAC and asymmetric families is
  rejected.** `KRAB_JWT_ALLOWED_ALGS=HS256,RS256`-style configurations are
  the classic key-confusion footgun: with both families allowed against the
  same key set, a public RSA/EC verification key doubles as an HMAC secret.
  Startup now fails on a mixed allowlist outside dev, and the request path
  fails closed (503, `jwt_allowlist_mixes...` warning) in every environment
  rather than verifying anything under such a list. Single-family allowlists
  are unaffected.

- **Issuer/audience validation can no longer be silently absent in
  staging/prod.** `iss`/`aud` checks only run when the expected values are
  configured, so a deployment that never set them accepted tokens from any
  issuer, minted for any audience. With `KRAB_AUTH_MODE=jwt|oidc` outside
  dev, startup now requires `KRAB_OIDC_ISSUER` + `KRAB_OIDC_AUDIENCE` (the
  fallback-tuple path already did) and, when `KRAB_JWT_PROVIDERS_JSON` is
  used, that **every** provider declares non-empty `issuer` and `audience`.
  Dev behaviour is unchanged. Documented in `.env.example` and
  `docs/reference/environment.md`.

### Fixed

- **`Dockerfile.service` builds again.** The dependency-cache stage still
  copied the manifest of `krab_server`, a crate removed in 0.2.0 (ADR 0005),
  which failed every image build at `COPY`. The stage now copies the manifests
  of all ten current workspace members — including `service_users_split` and
  the `islands_rpc` example, whose absence made the cached stub build resolve
  nothing and silently skip dependency caching.

- **Docs no longer teach non-compiling `view!` code.** The `create_action`
  examples in the getting-started guide, the server-functions reference, and
  the `krab_core::action` module docs used a reactive `disabled={ move || … }`
  attribute closure, which does not compile — `view!` evaluates attribute
  values once at build time. The examples now reflect pending state through
  `<Show>`, and both docs state the attribute-reactivity limitation
  explicitly. The render-policy and benchmarks docs also now state precisely
  what `streaming` delivers today (chunked delivery of a complete render, not
  progressive rendering) and that the NFT gate measures serial, unloaded
  latency.

### Added

- **Community health files.** `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1,
  conduct reports to the maintainer), a pull-request template carrying the
  CONTRIBUTING verification checklist, and structured issue forms (bug /
  feature) whose config routes vulnerability reports to the private advisory
  flow instead of public issues.
- **`SECURITY.md`** — vulnerability reporting policy (private GitHub Security
  Advisories), supported-version table, and scope for the five published
  crates.
- **`docs/guides/troubleshooting.md`** and
  **`docs/architecture/core_runtime.md`** — developer troubleshooting
  (hydration, service startup, CI-gate feature mismatches) and a
  module-by-module map of `krab_core` with its cross-cutting invariants,
  migrated from the maintainer wiki so the public docs carry them.

- **DB-backed tests no longer pass silently when Postgres is unreachable.**
  The `krab_core` migration-governance suite early-returned on connection
  failure, so a runner with no database reported the whole suite green while
  executing nothing. A shared helper now prints an unmistakable
  `SKIPPED <test>: ... This test executed NOTHING.` line to stderr before
  returning, and setting `KRAB_REQUIRE_DB_TESTS=1` (CI mode) turns an
  unreachable database into a panic — the suite fails instead of
  greenwashing. Documented in `.env.example` and
  `docs/reference/environment.md`.

- **Generated project README and Dockerfile were dishonest about what
  works.** The `krab new` README told consumers to run `krab release certify
  --out release-evidence` — a governance command hardcoded to the framework's
  own `service_auth`/`service_users` and meaningless in a generated project.
  The Testing section now says `cargo test` and carries an explicit note that
  the governance commands operate on the framework workspace only. The
  generated Dockerfile also `COPY`d a `Cargo.lock` that a fresh scaffold does
  not have, failing every `docker build`; it now copies only `Cargo.toml`,
  with a comment on re-adding the lockfile once committed.

- **`krab gen service` output could not compile.** The generated manifest
  declared `krab_core = { path = "../krab_core" }` — a directory that has not
  existed since the crates/ reorganisation — and the generated `main.rs` used
  `#[async_trait]` without the manifest declaring `async-trait`. Both the
  single-crate and split-topology generators now emit a registry dependency
  pinned to the CLI's own workspace version (the same resolution `krab new`
  uses) and declare `async-trait`; the unused `ServiceConfig` import is gone.
  Tests parse the generated manifests and pin the dependency set, and verify
  every feature the generator can request exists in `krab_core`. Note the
  registry dependency resolves once the workspace version is published; for
  building generated output against a local checkout, use `krab new
  --path-deps`.

- **Migration checksums survive a Rust toolchain bump.** They were computed
  with std's `DefaultHasher`, whose output is documented as unstable across
  Rust releases — the next toolchain bump would have flagged every previously
  applied migration as drifted. Checksums are now SHA-256 (hex, 64 chars).
  Rows written in the old format are recognised (same SQL, legacy hash) and
  rewritten to SHA-256 in place during the next migration run or drift check
  instead of being reported as drift; a genuine mismatch still fails. The
  legacy hash is only reproducible on the toolchain that wrote it, so run one
  migration pass or `krab db drift` before bumping the toolchain to upgrade
  existing databases. See `docs/reference/database.md`.

- **Every `/internal` request through `apply_common_http_layers` was 403.**
  Axum layers wrap bottom-up, so `service_auth_middleware` — which validates
  the service scope on the `AuthContext` request extension — ran *before*
  `auth_middleware` had inserted that extension. The scope check therefore
  never saw an authenticated caller, valid token or not. The layer order is
  swapped so authentication runs first on the request path, with a regression
  test driving a scoped token through the full production layer stack to an
  `/internal` route (and a second proving the scope gate still rejects tokens
  without the service scope).

- **A broken `DATABASE_URL` secret source silently fell back to the localhost
  default.** `DbConfig::from_env` swallowed `read_env_or_file` errors with
  `.ok()`, so an unreadable `DATABASE_URL_FILE` or unresolved
  `DATABASE_URL_VAULT_REF` produced a service happily connecting to
  `postgres://postgres@localhost:5432/krab` instead of failing. It now returns
  `anyhow::Result` and propagates sourcing errors; service startups already
  return `Result`, so a broken secret source aborts boot with the failing
  variable named. A merely *unset* `DATABASE_URL` still falls back to the
  driver default for dev convenience. **Breaking** for direct callers of
  `DbConfig::from_env` (unpublished `0.2.0` API): the return type gained
  `Result`.

- **The shipped wasm client could never satisfy CSRF protection.** The client
  half of `#[server]` read a cookie named `csrf_token` from `document.cookie`,
  while the server middleware set `krab_csrf_token` — and set it `HttpOnly`,
  so script could not read it under any name. Every state-changing server-fn
  call from a browser was doomed to 403 the moment CSRF protection was
  enabled. The client now fetches the token from the CSRF token endpoint's
  JSON body (mount `csrf_token_endpoint` at `krab_core::csrf::
  CSRF_TOKEN_ENDPOINT_PATH`, `/api/csrf-token`); the cookie stays `HttpOnly`.
  Cookie name, header name, endpoint path, and JSON field are now shared
  constants in `krab_core::csrf` referenced by both halves, with a native test
  pinning the endpoint's output to them, so the names cannot drift again.
  Deployments that do not enable CSRF protection are unaffected: when the
  endpoint is not mounted the client sends no CSRF header, as before.

- **`krab release check --json` exited 0 on failure.** The `--json` branch
  printed the report and returned success, so any pipeline gating on the exit
  code shipped through failed checks. Both output modes now share one exit
  path: a failed report is a non-zero exit, and `--json` only changes what is
  printed. In the same pass: an invalid `KRAB_PORT` no longer panics the
  release check (`KrabConfig::from_env_checked` is used and a bad value is
  reported as a failed `secrets_policy` check), and `release check` /
  `release certify` no longer leak `KRAB_ENVIRONMENT=prod` into later gates
  running in the same process — the prior value is saved and restored.

### Changed

- **First-public-release normalization.** Repository, homepage, clone, and
  rustdoc URLs unified on `github.com/ManirajKatuwal/krab-pub`; README gained
  badges, a completed documentation map, and a consistent
  not-yet-on-crates.io installation story; generated artifacts
  (`docker/composed.nft.rendered.yaml`, `service_frontend/public/__ssg/`)
  and maintainer-local agent tooling (`.claude/`) are no longer tracked;
  references to the untracked `internal/` planning tree were removed or
  annotated as not distributed throughout docs, comments, and CI workflows.

- **Dev profile no longer emits debuginfo for dependency crates**
  (`[profile.dev.package."*"] debug = false`), cutting dev target size and
  link time. Workspace crates keep full debuginfo: Cargo excludes workspace
  members (path dependencies) from the `"*"` package spec, so `[profile.dev]
  debug = true` still applies to them.

<!-- The categories below continue the 0.2.0 release notes. -->


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
- **`krab_core::loading` (`LoadingState`, `LoadingFallback`, `RouteTransition`)
  is deleted.** Dead surface: nothing in the workspace, the services, or the
  reference app ever constructed any of it — its only executions were its own
  unit tests. [ADR 0009](docs/adr/0009-resource-ssr-semantics.md) flagged the
  module as remove-or-use when `Resource` landed, and `Resource` already covers
  the ergonomics: `ResourceState::Pending` is the loading state a component
  actually renders against, with `state`/`value` as live signals instead of a
  hand-driven string-rendering state machine. The crate is unpublished, so no
  consumer is affected.

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

- **`wasm-pack build` died at the `wasm-opt` step on current Rust.** rustc 1.97
  emits bulk-memory and nontrapping-fptoint operations by default; the binaryen
  build that wasm-pack 0.13 bundles (version 117) rejects them as invalid input
  unless the features are explicitly enabled. Both wasm bundles — the
  `krab_client` runtime and the reference app — now pass
  `--enable-bulk-memory --enable-nontrapping-float-to-int` to `wasm-opt` via
  `[package.metadata.wasm-pack.profile.release]`. Found by the first
  containerised execution of the `reference-app` workflow's command set: the
  failure was latent in `reference-app.yaml` and `wasm-size.yaml` both, and
  would have failed CI's first-ever real run.

- **`<Show>` and `<For>` in `view!`.** The macro had no conditionals and no
  iteration: a list was an interpolated closure returning a `Fragment`, and the
  keys that make reconciliation work had to be stamped by hand, so a user who
  did not know `data-krab-node-id` existed got positional matching and a full
  rebuild on every change.

  `<For each={…} key={…} view={…}/>` stamps the key for you, which is the whole
  point of it over a hand-written `map`. Omitting `key` is a compile error
  rather than a silent fallback. `<Show when={…} fallback={…}>` renders nothing
  when no fallback is given.

  These are the only capitalised tags `view!` accepts; every other one remains
  the compile error [ADR 0006](docs/adr/0006-view-component-composition.md)
  introduced, and the message now lists the built-ins.
  [ADR 0008](docs/adr/0008-view-control-flow-tags.md) records why a closed set
  refines that rule rather than reversing it.

- **A `Dynamic` tracked one DOM node, but a `Fragment` renders several.** This
  broke `<Show>` and `<For>` in two different ways. When built on the client,
  the retained node was the `DocumentFragment` — which empties itself into the
  parent on append — so `parent_node()` was `None` and updates were silently
  skipped. When hydrated, the retained node was the *first* row, so an update
  replaced it with a fragment of the whole new list and left the remaining old
  rows behind: a server-rendered list **duplicated** rather than froze.

  Both paths now track the run of nodes a `Dynamic` owns and reconcile it
  through the same keyed reconciler, bounded by a trailing comment anchor so a
  `<For>` beside other children touches only its own rows. The anchor is created
  on first update, not during hydration — inserting it mid-traversal shifts the
  live `NodeList` and reports every following sibling as a mismatch.

- **A changed child count rebuilt the whole subtree.** `patch_dom` reconciled
  children only while `old.children.len() == new.children.len()`, and returned
  `None` otherwise — which made the caller destroy and recreate the entire
  element. Adding one row to a list therefore rebuilt every row, discarding DOM
  identity, focus, selection, and scroll position. `(Fragment, Fragment)` was
  not handled at all and took the same path.

  Children are now reconciled by key: fragments are flattened first, so keys
  match across a fragment boundary — the shape a list-rendering `Dynamic`
  produces — keyed nodes are moved rather than rebuilt, and unkeyed ones fall
  back to positional matching. Keys reuse `data-krab-node-id`, the marker
  `annotate_hydration_tree` already stamps, rather than adding a second notion
  of identity. Seven browser tests assert DOM node *identity* across updates,
  including that a focused input keeps its focus and its typed value when a row
  is inserted above it.

- **A successful patch panicked with `RefCell already borrowed`.** Both
  `Dynamic` sites held `current_node.borrow()` across the
  `*current_node.borrow_mut() = patched_node` that a successful patch performs.
  It was nearly unreachable while patching required an exact child-count match;
  keyed reconciliation made success the normal path and it fired immediately.

- **Effects leaked, and the leaked ones kept running.** `create_effect` pushed
  every effect into a thread-local `ROOT_EFFECTS` that was only ever appended
  to, holding a strong `Rc` forever. `create_dom_node` calls `create_effect` for
  every nested `Dynamic` it builds, so each re-render of a parent `Dynamic` left
  the previous run's effects alive, still subscribed, and still patching DOM
  nodes that had already been detached — ten updates meant ten zombie effects
  doing work on invisible nodes. This was wrong behaviour, not just growth.

  Effects now have owners: one created while another is running becomes that
  effect's child and is disposed when the parent re-runs. Only a genuinely
  top-level effect is retained for the life of the thread. A disposed effect
  never runs again even if a stale subscription still points at it.

- **`on_cleanup` added**, running when the owning effect re-runs or is disposed.
  Disposal needed the primitive internally, and without it there was no way to
  release anything an effect had acquired.

- **A signal read twice ran its effect twice, natively.** `get()` and `with()`
  subscribe on every read, and while the wasm path collapsed duplicates in
  `PENDING_EFFECTS`, the native path ran the effect once per subscription — so
  an effect reading a signal three times ran three times per write, forever.
  Deduplication moved to `notify()`, which both platforms share.

- **`krab_client`'s hydration tests were testing a mock of the algorithm, not
  the algorithm.** `hydration_plan`, `hydration_plan_children`, `HydrationPlan`,
  `HydrationOutcome`, and `element_hydration_id` were all `#[cfg(test)]` — a
  second, hand-maintained implementation of hydration that 13 tests exercised.
  The real functions are `#[cfg(feature = "web")]`, which a native `cargo test`
  never enables, so the shipping algorithm was never compiled during a test run,
  let alone executed.

  The two had already drifted. The runtime compares tags case-insensitively
  (`Element::tagName` is uppercase in a browser); the model compared them
  case-sensitively. The model also produced `expected_element_found_non_element_node`
  and `dynamic_node_boundary`, neither of which the runtime emits — and a test
  asserted the latter. The model is deleted; its two worthwhile cases (reordered
  marker-matched children, attribute-only differences not forcing a replacement)
  are now browser tests asserting DOM node identity against the real runtime.

- **`hydrate_recursive` is 101 lines rather than 324**, with the element and
  text paths, node replacement, and node appending extracted. `create_dom_node`
  drops from 182 to about 55. The two paths also carried a **verbatim duplicate**
  of the event-listener bookkeeping — closure creation, the `__krab_id` stamp,
  the `EVENT_CLOSURES` insert — which is now one `attach_element_events`; a fix
  applied to one copy would previously have missed the other. Behaviour is
  unchanged and verified by the browser suite before and after.

- **The `wasm-bindgen` family is updated** (`0.2.114` → `0.2.127`,
  `wasm-bindgen-test` `0.3.64` → `0.3.77`, plus `js-sys`, `web-sys`, and
  `wasm-bindgen-futures`). The older runner could not open a WebDriver session
  against ChromeDriver 151 — it failed parsing the `newSession` response
  (`invalid type: map, expected a string`) before any test body ran, which made
  the hydration browser tests impossible to execute on a current Chrome.

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

- **`create_memo`, `untrack`, `batch`, and `on_cleanup`.** The reactive system
  had only signals and effects: no derived-value caching, no way to read without
  subscribing, and no way to group writes. A derived closure read three times
  computed three times, and three writes ran a dependent effect three times.

  Memos are lazy and cached — a write marks dependents dirty without
  recomputing, and the value is recomputed on read. That ordering is what makes
  a diamond (`source → a`, `source → b`, `effect(a, b)`) settle **once** per
  write with both branches fresh; recomputing eagerly on notification would run
  the effect once per branch, the first time seeing one updated and one stale
  value. A memo nothing reads costs nothing to keep current.

  `batch(f)` groups writes so dependent effects run once at the end; nesting is
  safe and only the outermost scope flushes. It guarantees the run *count*, not
  the moment — on wasm the flush uses the same microtask queue an unbatched
  write already used.

  Memos created inside an effect are owned by it and disposed when it re-runs,
  matching nested effects.

- **`krab_core::action::Action` and `create_action`**, re-exported as
  `krab_client::Action`. Calling a `#[server]` function from an island worked,
  but everything around the call had to be hand-rolled: a signal for "is it
  running", another for the result, another for the error, and the discipline to
  clear them in the right order. `create_action` wraps an async operation and
  exposes `pending`, `value`, and `error` as signals, so a button can disable
  itself and an error can render without any bookkeeping in the handler.

  Two behaviours are chosen deliberately, because the obvious implementation
  gets them wrong. A **failed dispatch keeps the previous value** rather than
  clearing it — blanking rendered data because a retry failed is worse than
  showing the last good value beside the error. And **only the newest dispatch
  can write state**: a generation counter discards the response of a superseded
  call, so two clicks where the first request finishes second leave the newer
  answer on screen rather than the older one with `pending` reading false, which
  looks settled and is wrong.

  It lives in `krab_core`, not `krab_client`, because an `#[island]` body is
  compiled for **both** targets — natively to render the markup, on wasm to
  hydrate it — so an API available only on wasm forces a `#[cfg(target_arch)]`
  back into application code. Natively `dispatch` returns without touching a
  signal, so server-side rendering produces the idle markup the browser
  hydrates against.

- **`krab_core::resource::Resource` and `create_resource`** — the read-side
  counterpart to `Action`, per [ADR 0009](docs/adr/0009-resource-ssr-semantics.md).
  A resource tracks a source closure, runs an async fetcher when it changes,
  and exposes `state()` (`Pending` / `Ready` / `Error`) and `value()` as
  signals. `value` deliberately survives both a refetch (`state` shows
  `Pending`, the data stays on screen) and a failed refetch (`state` shows the
  error, the last good value stays), and a generation counter discards
  superseded responses.

  **SSR semantics, decided by ADR 0009:** a resource never polls its future on
  the server. `create_resource_with_initial` takes a server-fetched value —
  typically through island props from the async route handler — renders
  `Ready`, and does **not** refetch on mount, so no load is doubled. Without an
  initial value the server renders `Pending` and the client fetches after
  hydration. Blocking the render was rejected (`Node` is `!Send`; the tree is
  built synchronously); streaming is deferred with the initial-value path
  reserved as its integration point.

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

- **The hydration runtime has test coverage for the first time.**
  `crates/framework/krab_client/tests/hydration_browser.rs` runs the real
  algorithm in headless Chrome (7 tests), with a `smoke_browser.rs` canary (3)
  that distinguishes a broken harness from a broken runtime. Both run in the new
  `client-browser-tests` CI job.

  `cargo test --workspace` compiles `krab_client` for the host, where there is
  no `document`, so it could only ever reach pure functions — every DOM-mutating
  path (`hydrate`, `hydrate_recursive`, `create_dom_node`, `patch_dom`) had no
  test at all, and a hydration defect surfaces as a subtly wrong DOM in a user's
  browser rather than a red build. The tests cover node reuse, mismatch patching
  and counting, unregistered islands, malformed props not aborting sibling
  islands, idempotency, and removal of stale server-rendered children.

  `.cargo/config.toml` now routes the `wasm32-unknown-unknown` target through
  `wasm-bindgen-test-runner`, so exactly one `wasm-bindgen` version is in play —
  the one Cargo resolved — rather than the separate copy `wasm-pack` bundles.

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
- **[`docs/reference/benchmarks.md`](docs/reference/benchmarks.md)** — the
  benchmark/NFT methodology and every committed result snapshot in one lookup
  page: what each harness in `scripts/` measures, the release-blocking limits
  in `benchmarks/thresholds.json`, the `N=1` vs `N=3` scaling gate, the
  external-comparison matrix, and exact reproduction commands. The committed
  snapshots previously carried numbers with no public statement of method or
  caveats — including that the 2026-03-08 "comparison" run measured Krab only,
  on a local machine, against a size-optimised (`opt-level = "z"`) build.
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
> `cargo audit`, and `cargo deny check advisories licenses bans sources`
> passed at HEAD; the evidence bundle is recorded in
> `internal/audit/evidence/2026-08-07_remediation/` (maintainer-local,
> gitignored).
>
> Link-dependent gates were deferred to a later verification pass:
> `cargo test --workspace`, the feature-gated `krab_core` suites,
> `cargo clippy --all-targets`, `cargo doc`, and the `krab` governance
> commands. This section therefore records merged changes as of that date
> rather than a full-gate attestation. Deferred-gate status is tracked in
> `internal/audit/VERIFICATION_EVIDENCE_LOG.md` §7 (maintainer-local,
> gitignored).

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
