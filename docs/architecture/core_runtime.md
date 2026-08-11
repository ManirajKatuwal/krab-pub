# Core Runtime (`krab_core`)

[`krab_core`](../../crates/framework/krab_core/) is the platform layer every
other crate and service builds on: the view tree and its rendering, the
reactive system, configuration and secret-sourcing policy, the HTTP runtime,
database governance, and the resilience and telemetry primitives services
share. This page maps the crate module by module, as declared in
[`src/lib.rs`](../../crates/framework/krab_core/src/lib.rs).

Two gating mechanisms are in play, and they are not the same thing:

- **Cargo features** decide what is compiled at all. `krab_core` ships **no
  default features** — a bare build is the view/reactivity core only.
- **Target gates** (`#[cfg(not(target_arch = "wasm32"))]`) exclude
  server-only modules from browser bundles regardless of features, because
  they depend on `tokio`.

| Feature | Unlocks |
|---|---|
| `rest` | Axum HTTP layer: `http`, `http_auth`, `http_headers`, `http_runtime`, `http_security`, `http_error`, `static_assets`, and the server half of `server_fn` |
| `graphql` | `graphql` (`async-graphql` execution policy) |
| `auth` | `credentials` (Argon2id password verification) — transport-independent, does not imply `rest` |
| `db-postgres` | `db` + `repository`, with the full Postgres migration-governance runtime |
| `db-sqlite` | `db` + `repository`, SQLite driver only (migration governance is Postgres-specific) |
| `redis-store` | `RedisStore` backend inside `store` |
| `web` | Browser bindings (`web-sys`/`wasm-bindgen`) — the wasm32 half of islands and `server_fn` |
| `grpc-semantics` | `grpc_semantics` — status-code and `grpc-timeout` vocabulary, **not** a transport ([ADR 0007](../adr/0007-grpc-feature-disposition.md)) |

Deprecated aliases, removable no earlier than 0.3.0: `db` → `db-postgres`,
`grpc` → `grpc-semantics` (the module alias `krab_core::grpc` is likewise
`#[deprecated]`).

---

## The crate root

The view tree itself lives directly in
[`lib.rs`](../../crates/framework/krab_core/src/lib.rs): `Node` (element /
text / fragment / dynamic), `Element`, `Attribute`, `EventListener`, the
`Render` trait that turns a tree into an HTML string (with escaping applied to
text and attribute values), and `IntoNode` conversions used by `view!`
interpolation. It also owns hydration annotation: `annotate_hydration_tree`
stamps every element with a stable `data-krab-node-id` path
(`HydrationNodeMarker`), stopping at nested `data-island` boundaries so each
island owns its own subtree ([ADR 0001](../adr/0001-hydration-markers.md)).

`Node` is **`!Send`** — the `Dynamic` variant and event listeners hold `Rc`.
Build and consume it inside synchronous sections of async handlers; never hold
one across an `.await`. See [signal_safety.md](signal_safety.md).

## View and rendering

| Module | Gate | Responsibility |
|---|---|---|
| `control_flow` | — | Runtime behind `view!`'s `<Show>` and `<For>` tags ([ADR 0008](../adr/0008-view-control-flow-tags.md)). Both return `Node::Dynamic`, so updates flow through the same effect machinery as any other reactive interpolation. |
| `error_boundary` | — | Catches panics during render (`catch_unwind`) at a boundary, substitutes a deterministic fallback node, and records a `BoundaryDiagnostic` instead of taking down the whole render. |
| `head` | — | Per-route `<head>` metadata with deterministic merge semantics for nested layout composition. |
| `layout` | — | Composable layout wrappers for nested route rendering, cooperating with `head` for metadata merge. |
| `image` | — | Typed `<img>` construction (`ImageProps`) with dimension and loading attributes. |
| `style_scope` | — | Scoped-CSS artifacts: per-component scope ids and generated class names. |
| `i18n` | — | Locale routing and translation bundles (`I18n`, `Locale`, `TranslationBundle`). |
| `render_policy` | — | Per-route render mode (`Static` / `Server` / `ClientOnly`) and revalidation policy ([ADR 0002](../adr/0002-render-policy.md), [render_policy.md](render_policy.md)). |
| `render_stream` | — | Chunked SSR delivery: suspense-state markers (`Pending` / `Resolved`) stamped into synchronously rendered output — progressive client consumption does not exist yet ([render_policy.md](render_policy.md)). |
| `isr` | not wasm32 | Incremental Static Regeneration: cached page refresh without full rebuild, backed by `store::DistributedStore` (in-memory by default, Redis for multi-replica). |
| `static_assets` | `rest` | Safe static file serving; `resolve_static_pkg_path` canonicalises and rejects traversal/absolute paths before serving `pkg/` bundles (ported from the removed `krab_server`, [ADR 0005](../adr/0005-krab-server-disposition.md)). |

## Reactivity and data flow

| Module | Gate | Responsibility |
|---|---|---|
| `signal` | — | The reactive core: `create_signal`, effects, memos, batching. **Single-threaded by construction** — `Rc`/`RefCell` internals make signals `!Send`, so cross-thread misuse is a compile error, not a runtime one ([signal_safety.md](signal_safety.md)). |
| `action` | — | Client-initiated async **writes** with observable state: `create_action` packages pending/value/error signals, keeps the last good value through a failed retry, and lets only the newest dispatch write. |
| `resource` | — | Client-driven async **reads**: `create_resource` tracks a source, refetches when it changes, and never polls its future during SSR ([ADR 0009](../adr/0009-resource-ssr-semantics.md)). |
| `server_fn` | `rest` or `web` | The `#[server]` runtime: server-side handler plumbing and `ServerFnError` under `rest`; `call_server_fn` (the fetch stub the macro's wasm32 half calls) under `web`. Available to both halves deliberately — gating it on `rest` alone would make the client half unbuildable. |
| `store` | not wasm32 | `DistributedStore` trait with `MemoryStore` always available and `RedisStore` under the `redis-store` feature; the shared state behind ISR and other cross-replica concerns. |

## HTTP runtime (all `rest`)

| Module | Responsibility |
|---|---|
| `http` | `apply_common_http_layers` — the middleware stack every service applies, composing the modules below in a fixed order — plus the global per-IP token-bucket rate limiter. |
| `http_auth` | Bearer/JWT/OIDC request authentication; fails closed on invalid credentials. Static-asset paths (`/pkg/`) are exempt. |
| `http_headers` | CORS and security headers (`Strict-Transport-Security`, `X-Content-Type-Options`, `X-Frame-Options`); invalid configuration maps to controlled responses, never a panic. |
| `http_runtime` | `RuntimeState` (including rate-limiter configuration) plus the operational endpoints: `/health`, `/ready` (with per-dependency status), and metrics payloads. |
| `http_security` | Client-IP extraction — forwarded headers are untrusted unless `KRAB_TRUST_PROXY_HEADERS=true` — and CSRF protection (cookie/header token pair). |
| `http_error` | Structured JSON error responses with stable, snake_case error codes. |
| `http_observability`, `http_protocol` | Private implementation details of tracing/metrics middleware and protocol negotiation — not public API. |

## Protocol and service composition

| Module | Gate | Responsibility |
|---|---|---|
| `protocol` | — | `ProtocolKind` and `ProtocolConfig` (explicit endpoint-based selection, [ADR 0004](../adr/0004-protocol-selection-by-explicit-endpoint.md)) and the versioned `RpcEnvelope` for additive schema evolution. `ProtocolKind::parse("grpc")` returns `None` by design. |
| `service_contract` | — | Topology-aware contract adapters: the same service contract served in-process (single) or over the network (split). See [service_composition.md](service_composition.md). |
| `graphql` | `graphql` | Framework-level GraphQL execution policy on `async-graphql`: query size limits, introspection control, error shaping. |
| `grpc_semantics` | `grpc-semantics` | gRPC status codes and `grpc-timeout` header parsing for a gateway boundary. No transport, no codegen, no `tonic`/`prost` ([ADR 0007](../adr/0007-grpc-feature-disposition.md)). |
| `ws` | not wasm32 | WebSocket ergonomics for Axum-based services: `WsMessage`, `WsRoom`, and broadcast-based room management on `tokio`. |

## Configuration, persistence, and operations

| Module | Gate | Responsibility |
|---|---|---|
| `config` | — | `KrabConfig::from_env_checked` (typed env parsing with defaults), `validate()` (environment-dependent security enforcement), `read_env_or_file` (the mandatory secret-sourcing path), and the secrets-source policy report. |
| `credentials` | `auth` | `CredentialStore` — the trait an application implements for password verification — and `EnvHashCredentialStore`, reading Argon2id PHC hashes from the environment. |
| `db` | `db-postgres` or `db-sqlite` | A **module directory** ([`src/db/`](../../crates/framework/krab_core/src/db/)): `driver.rs` resolves `KRAB_DB_DRIVER=postgres\|sqlite` (driver-agnostic — compiling a driver in and selecting one at runtime are separate); `postgres.rs` holds pooling plus the migration-governance runtime — checksums, drift detection, rollback, promotion policy ([database.md](../reference/database.md)). |
| `repository` | `db-postgres` or `db-sqlite` | `UserRepository`, the persistence port applications implement per driver — deliberately not Postgres-specific. |
| `resilience` | — | Circuit breaker (`Closed` / `Open` / `HalfOpen`) and retry/backoff helpers; services and the orchestrator use these instead of ad-hoc loops. |
| `telemetry` | not wasm32 | `tracing` initialization and the standard startup fields (service name, environment) so log aggregators can rely on stable field names. |
| `service` | not wasm32 | `ApiService` trait and `ServiceConfig` — the shared boot path services run under. |

---

## Cross-cutting invariants

These hold across every module; changes that break one are rejected in review
or by CI.

1. **Startup never panics.** Boot paths return `anyhow::Result` and propagate;
   `KrabConfig::from_env` (the panicking convenience) exists only where
   invalid config is unrecoverable — new code uses `from_env_checked`.
2. **Validation is environment-dependent, fail-fast, and fail-closed.** Dev
   skips checks; staging/prod/unknown environments reject wildcard CORS,
   static auth, and inline secrets at startup, before serving a request.
3. **Secrets go through `config::read_env_or_file`.** Inline values are
   rejected outside dev; `*_FILE` / `*_VAULT_REF` sourcing is the supported
   path.
4. **Rendering escapes by default.** `Render` escapes text and attribute
   values; there is no raw-HTML interpolation path in the view tree.
5. **Hydration markers are stable contract.** `data-krab-node-id` paths and
   island boundary attributes are relied on by `krab_client` and by tests;
   annotation never overwrites an existing marker.
6. **Reactive types are `!Send`.** `Node`, signals, actions, and resources are
   single-threaded by type; the compiler is the enforcement mechanism.
7. **Feature gates are honest.** A module compiled out is absent, not stubbed.
   Match the CI feature set before trusting a local test run (see the
   [troubleshooting guide](../guides/troubleshooting.md)).

## Extension patterns

Verified against current code — follow the existing shape rather than
inventing a parallel one.

**Add a configuration knob**

1. Add the field to the config struct in
   [`config.rs`](../../crates/framework/krab_core/src/config.rs).
2. Parse it with a default in `from_env` / `from_env_checked` — absent must
   mean the documented default, never an error.
3. If misconfiguration is dangerous, enforce in `validate()` with an error
   message that names the variable and the environment.
4. Test both paths: the default and the invalid value (see the existing
   `from_env_rejects_invalid_port_value`-style tests).
5. Document it in [`.env.example`](../../.env.example) and
   [environment.md](../reference/environment.md) **in the same change** — this
   pairing is an enforced convention.

**Add a secret**

Read it via `read_env_or_file`, and add it to the well-known list in
`KrabConfig::SECRET_VARS` so the secrets-source policy covers it. Never read a
secret with a bare `std::env::var`.

**Add a feature-gated module**

Declare the feature in
[`Cargo.toml`](../../crates/framework/krab_core/Cargo.toml) with its optional
dependencies, gate the `pub mod` declaration in `lib.rs`, and gate its tests
(`#[cfg(all(feature = "...", test))]`). If the module must exist in the
browser bundle, remember the two gates are independent — `server_fn`'s
`any(feature = "rest", feature = "web")` gate is the worked example of getting
this right.

**Add an HTTP middleware concern**

New middleware goes in its own `http_*.rs` module and is composed in
`http::apply_common_http_layers`, keeping ordering decisions in one place.
Header parsing must not panic, and failures must map to controlled responses.

---

## Related pages

- [design.md](design.md) — the whole-framework architecture this crate anchors
- [hydration.md](hydration.md) — the SSR-to-island pipeline built on the crate root's markers
- [signal_safety.md](signal_safety.md) — why the reactive types are `!Send` and what that implies
- [render_policy.md](render_policy.md) — the policy surface behind `render_policy`
- [service_composition.md](service_composition.md) — `service_contract` in single and split topology
- [database.md](../reference/database.md) — the governance rules `db` enforces
- [security.md](../reference/security.md) — the policy `config` and the `http_*` stack implement
