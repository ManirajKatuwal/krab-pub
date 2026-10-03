# Migration Guide

This guide maps common framework concepts to Krab equivalents. It is intentionally conceptual; use the reference apps for concrete layouts.

---

## Upgrading within Krab

### 0.5.0 → 0.6.0

0.6.0 removes everything that was deprecated with a stated removal version, plus
three older deprecations that had none. Each was a compiler warning on 0.5.x, so
a crate that builds warning-free on 0.5.0 has nothing to change for the removals
below. Additive changes to public types can still break a build — see
[Public types that grew](#public-types-that-grew).

| Removed | Replace with |
|---|---|
| `krab_client` feature `demo-islands`, `krab_client::components::{Counter, Toggle, Likes}` | Your own islands, defined with `#[island]` in your crate (see below) |
| `krab_core::db::postgres::run_migrations(&pool)` | `run_versioned_migrations(&pool, &migrations, MigrationFailurePolicy::Halt)` |
| `krab_core::render_stream::SuspenseMarker::parse(..)` | `is_finalized_ssr_snapshot(&html)` |
| `krab_core` feature `db` / `grpc` | `db-postgres` / `grpc-semantics` |
| `krab_core::grpc` | `krab_core::grpc_semantics` |
| `KrabConfig::from_env(name, port)` | `KrabConfig::from_env_checked(name, port)?` |
| `room.connect().await` … `room.disconnect().await` | `let _guard = room.join();` — dropping the guard disconnects |

#### If you used the demo islands

**What breaks:** a crate that named `Counter`, `Toggle` or `Likes` from
`krab_client`, or enabled `features = ["demo-islands"]`, no longer compiles.

**Fix:** copy the component into your own crate. The macro is the whole
mechanism — there is nothing to inherit:

```rust,ignore
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct CounterProps { pub initial: i32 }

#[island]
pub fn Counter(props: CounterProps) -> Node {
    let (count, _set_count) = create_signal(props.initial);
    view! {
        <button on:click={ move |_| _set_count.update(|c| *c += 1) }>
            "Count: " <span>{ move || count.get().into_node() }</span>
        </button>
    }
}
```

The crate that defines islands must also be the crate you build for wasm32
(with its own `web` feature on), because `#[island]` registers the hydrating
half in the crate where it is written. `krab_client`'s own bundle no longer
contains any island, so loading it on a page hydrates nothing.
`services/service_frontend_islands` is a complete example of the split: the
server links it without `web` for SSR markup, and `wasm-pack build` turns `web`
on for the bundle.

If you depended on `krab_client` with `default-features = false` only to avoid
the demo islands, that still works and is still a fine way to be explicit.

#### Public types that grew

- **`krab_core::Attribute` has a new public field, `dynamic`.** Struct literals
  need `dynamic: None`, or use `Attribute::new(name, value)`.
- **`krab_core::Node` has a new variant, `Comment`** (the `<Suspense>` markers).
  An exhaustive `match` on `Node` needs an arm for it.
- **`Suspense` is a reserved tag name in `view!`**, like `Show` and `For`. A
  component of your own called `Suspense` must be renamed or called by path
  (`<ui::Suspense/>`).
- **`RuntimeState` has new public fields** (`latency_sum_micros`,
  `auth_policy`, `auth_open_paths`, `auth_failure_reasons`, `code_public_paths`,
  `auth_open_paths_explicit`). Code that built it with a struct literal must add
  them; `RuntimeState::new()` / `try_new()` are unaffected. `public_paths` now
  holds only `KRAB_AUTH_PUBLIC_PATHS`; check a path with
  `RuntimeState::is_public_path`, which also consults the code-declared list.
- **`JwtProviderConfig` has new fields** (`jwks_url`, `key_not_after`, both
  `#[serde(default)]`). JSON configuration is unaffected; struct literals need
  `..` or the fields.

#### `<script>` and `<style>` content is raw text

`view!` used to HTML-escape the children of `<script>` and `<style>`, which broke
any script containing `=>`, `&&` or `<`. They are now emitted raw, with only
`</script`, `</style` and (in scripts) `<!--` neutralised so the content cannot
end the element early. Two consequences:

- Remove any workaround that avoided those characters, or pre-escaped content
  for the old behaviour — it is now emitted literally.
- **Do not interpolate untrusted strings into a `<script>` body.** Escaping no
  longer applies there; anything the rule above does not neutralise is live
  JavaScript. Pass data in a `<script type="application/json">` block
  (JSON-encoded) or a `data-*` attribute instead. Krab's CSP blocks inline
  executable scripts anyway: load code from a same-origin file.

`ScriptTag` inline content gains the `<!--` rule, and `LinkTag` / `ScriptTag`
`extra_attrs` now drop invalid attribute names and `on*` event-handler names.

#### Other changes worth checking

- **An explicit `KRAB_AUTH_OPEN_PATHS` overrides code-declared public paths.**
  When it is set, paths a service declares with
  `RuntimeState::with_public_paths` are ignored and a warning,
  `auth_code_public_paths_ignored`, names them. The reference frontend now
  declares its public routes that way, including `/_krab/home.js`,
  `/_krab/stream.js` and `/streaming`: a deployment that sets
  `KRAB_AUTH_OPEN_PATHS` must list those too (or add them with the additive
  `KRAB_AUTH_PUBLIC_PATHS`), or the home page cannot load its hydration runtime.
- **JWKS with the default algorithm allowlist fails startup outside dev.** A
  JWKS URL (`KRAB_OIDC_JWKS_URL` or a provider `jwks_url`) combined with an
  HMAC-only `KRAB_JWT_ALLOWED_ALGS` — including the `HS256` default — is now
  rejected by `KrabConfig::validate`. Set `KRAB_JWT_ALLOWED_ALGS` to the
  provider's algorithm (e.g. `RS256`).
- **`service_auth` reads its signing configuration once, at startup.** It used
  to re-read the `KRAB_JWT_*` key material (key files included) on every
  request, so replacing a mounted key file took effect immediately. Restart the
  service after rotating keys.
- **Kid-less JWTs.** A token without a `kid` is now tried against the configured
  keys (up to 8, default first) instead of one; set `KRAB_JWT_REQUIRE_KID=true`
  if your issuer always sets a `kid`. A kid-less token from an address already
  over its auth-failure budget is answered `429` before verification.

- **`krab db lifecycle|rollback|drift` need Postgres.** They reported success
  without one. Point them at a database (`KRAB_TEST_DATABASE_URL`) or stop
  running them where there is none.
- **Latency metric name.** `krab_request_duration_seconds_bucket` is now
  `krab_http_request_duration_seconds_bucket` (with `_sum` and `_count`). The old
  name is still emitted through 0.6.x — move dashboards and recording rules over
  before 0.7.0. The shipped alert rules already use the new name.
- **Auth-failure limiter outage status.** A store outage during an auth failure
  is 503, not 429. Alerting that keyed on 429s during Redis incidents should
  look at 503s. Only 401-class failures count against the budget now; a provider
  outage (503) or misconfiguration (500) no longer spends it.
- **`MemoryStore::incr` on a non-numeric value errors** instead of overwriting
  it. Only affects code that shared a key between a counter and something else.
- **Default open paths (deprecation).** A startup warning,
  `auth_legacy_default_open_paths_in_use`, lists application routes that are
  unauthenticated only because the framework's default list names them. Declare
  the ones your service serves, then opt in to the 0.7.0 default:

  ```sh
  KRAB_AUTH_PUBLIC_PATHS=/,/blog/*,/pkg/*
  KRAB_AUTH_LEGACY_OPEN_PATHS=false
  ```

  or in code: `RuntimeState::try_new()?.with_public_paths(["/", "/blog/*"])`.
- **`init_tracing(name)` is deprecated.** Replace it with
  `init_tracing_with_version(name, env!("CARGO_PKG_VERSION"))`; the old call logs
  `krab_core`'s version as your service's.
- **Also deprecated, removed in 0.7.0:** `krab_core::image` (`optimized_image`,
  `ImageProps`) — write the `<picture>` with `view!` against variants your
  pipeline produces; `krab_core::style_scope`; and
  `krab_core::telemetry::{RequestTelemetry, RedMetrics, EndpointMetrics}` —
  nothing reads them, drop them.
- **A mistyped `KRAB_ENVIRONMENT` needs `KRAB_CORS_ORIGINS`.** An unrecognised
  value already got prod secret rules; it now also refuses to start without an
  explicit CORS allowlist. Fix the value, or set `KRAB_CORS_ORIGINS`.
- **Unusual `x-request-id` values are replaced.** An inbound id longer than 128
  bytes or outside `[A-Za-z0-9._:-]` gets a fresh id instead of being echoed.
  If a caller correlates on its own ids, keep them within that alphabet.
- **The `+Inf` latency bucket counts completed requests** (it was set to
  `krab_requests_total`, which includes in-flight ones), and
  `StreamTelemetry::first_visible_chunk_ms` is now the first flush with paintable
  text rather than always equal to `ttfb_ms`. Recalibrate anything tuned to the
  old values.
- **GraphQL introspection blocking parses the query.** Ordinary queries that
  merely mention `__type` in a string or comment are no longer refused.
- **`UsersServiceContract` gains `get_user_on_behalf_of(id, authorization)`**,
  with a default that calls `get_user`; implementations keep compiling.

#### Browser bundles

- **Serve the bundle's `snippets/` directory.** `krab_client`'s per-island
  panic isolation ships as a JS snippet that `wasm-pack` writes under
  `pkg/snippets/`. Serve it beside the glue `.js` file (copying the whole `pkg/`
  directory does this); a bundle served without it fails to load.
- **The reference frontend's bundle is `service_frontend_islands.js`.** If you
  deploy `service_frontend` or copied its layout, build the islands crate
  instead of `krab_client`, serve it at `/pkg/service_frontend_islands.js`, and
  use manifest key `service_frontend_islands.js` (`krab.toml`'s
  `client_package` already names it). `/pkg/krab_client.js` hydrates nothing
  now — `krab_client` contains no islands.
- **If you copied the reference frontend's home page:** its hydration runtime is
  no longer an inline module script (Krab's CSP blocked it, so the page never
  hydrated under the framework's own headers). It is served at `/_krab/home.js`
  and configured by a `<script type="application/json">` block, with no import
  map. The hand-written deferred-hydration functions
  (`classifyIslandsForDeferredHydration`, `freezeCriticalIslandsAfterHydration`,
  `activateDeferredIslands`) and the `data-island-deferred`,
  `data-island-hydrated` and `data-krab-priority` attributes are gone; staging
  uses `hydrate_within_selector` (critical islands first, deferred ones on idle).

  ```sh
  wasm-pack build services/service_frontend_islands --release --target web --out-dir ../../dist/pkg -- --features web
  ```

#### `krab` CLI

- **Generated artifacts default to `.krab/`.** `krab db rehearsal`, `krab
  release certify` (without `--out`) and orchestrator service logs write under
  `.krab/` instead of `internal/audit/`. An existing `internal/audit/` directory
  is still used, with a deprecation warning, until 0.7.0. To choose the location
  explicitly — or keep the old one — set `KRAB_ARTIFACT_DIR` (for example
  `KRAB_ARTIFACT_DIR=internal/audit`). Add `.krab/` to your `.gitignore`;
  `krab new` already does.
- **Framework-only commands refuse outside the framework checkout.**
  `krab contract check|protocol-check`, `krab db lifecycle|rollback|drift|rehearsal`
  and `krab release check|certify` validate Krab's own reference services and
  now exit non-zero, with one message, in any other project. They never worked
  there. Use `cargo test`, `krab doctor --strict`, `krab topology doctor` and
  `krab security dependency-gate` in your own CI instead.
- **`krab doctor` and `krab env-check` read `./.env`.** Values already in the
  process environment still win. If a strict check starts failing, look at what
  your `.env` sets; in a fresh `krab new` project, `cp .env.example .env` is now
  enough for `krab doctor --strict` to pass.
- **`--path-deps` must name a Krab checkout.** `krab new --path-deps` and
  `krab gen service --path-deps` reject any other directory.
- **`krab topology split` skips ports `krab.toml` already assigns**, moving to
  the next free port in 3200–3499 and erroring if the range is full. A new
  split service can therefore get a different port than 0.5.0 would have given
  it; read the generated `krab.toml` entry rather than assuming the number.

#### Additive — nothing to change

Component tags in `view!` (`<Card title="x"/>`), the context API
(`provide_context` / `use_context`), reactive attributes
(`disabled={move || busy.get()}`), `<Suspense fallback={…}>`, progressive
streaming SSR (`render_to_stream`, `Resource::with_server_loader`),
`#[server(stream)]` on `wasm32`, remote
JWKS (`KRAB_OIDC_JWKS_URL`), scheduled key retirement
(`KRAB_JWT_KEY_NOT_AFTER_JSON`), `krab_auth_failures_by_reason_total`, and the
new `krab_client` exports (`hydrate_within_selector`, `hydrate_island`) are new
surface only. A capitalised tag in `view!` used to be a compile error, so no
existing code changes meaning.

### 0.4.0 → 0.5.0

Four breaking changes and one behaviour worth knowing about, in rough order of
how likely each is to page you.

#### Metrics endpoints are closed by default

**Breaking, operator-facing.** `/metrics` and `/metrics/prometheus` left the
default unauthenticated open-path list. Every Krab service was handing anonymous
callers its route inventory, request volumes, error counts and latency
histograms.

**What breaks if you do nothing:** an unauthenticated scraper that worked on
0.4.0 gets **401** after the upgrade. `/health` and `/ready` are unchanged and
still anonymous, so container liveness and readiness probes are unaffected.

**One-line fix**, if that surface is deliberately reachable only from a trusted
network:

```sh
KRAB_METRICS_PUBLIC=true
```

**Or keep it closed** and give the scraper a token:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: krab
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/krab-token
    static_configs:
      - targets: ["auth:3001", "users:3002", "frontend:3000"]
```

**Do not reopen metrics through `KRAB_AUTH_OPEN_PATHS`.** That variable
*replaces* the baseline open-path list rather than extending it, so a value of
`/metrics,/metrics/prometheus` also closes `/`, `/health`, `/ready`, the auth
endpoints, `/pkg/*` and every other default in the same breath.
`KRAB_METRICS_PUBLIC` is additive over whatever `KRAB_AUTH_OPEN_PATHS` resolves
to — set or unset — so the two knobs never interact, and one grep for
`KRAB_METRICS_PUBLIC` across an estate answers "who is exposing metrics?".

#### The orchestrator owns service ports and names

**Breaking for anyone with their own `krab.toml`.** `[services.X]` gains typed
`port` and `service_name` fields; the orchestrator resolves them and injects
them into each child as `KRAB_PORT` and `KRAB_SERVICE_NAME` at spawn.
Precedence, lowest first: the inherited environment, then the injected identity,
then explicit `[services.X].env` entries. See
[ADR 0012](../adr/0012-orchestrator-owns-service-identity.md).

```toml
[services.api]
command = "cargo"
args = ["run", "--bin", "backend"]
port = 3001
service_name = "backend"   # keeps the identity this binary reported on 0.4.0
healthcheck_url = "http://127.0.0.1:3001/health"
```

Three things to check before deploying:

- **Telemetry identity moves unless you declare it.** `service_name` defaults to
  the `[services.<key>]` table key. A `[services.api]` entry running a binary
  that called itself `backend` reports `service=api` on every log line, metric
  and migration record after the upgrade. Dashboards, saved queries and alert
  rules that filter on `service` are what breaks. Declare
  `service_name = "backend"` to keep the old value.
- **Duplicate ports and duplicate resolved names are now startup errors.** Two
  services declaring the same `port`, two resolving to the same `service_name`,
  and a `port` outside 1-65535 are all reported by name before anything is
  spawned. `krab topology doctor` runs the same checks statically, so put it in
  CI and find this before a deploy does:

  ```sh
  krab topology doctor --diagnostics
  ```

  A declared `port` that disagrees with the port in that service's probe URL is
  warned about, not rejected — a probe may legitimately address a proxy.
- **`krab.yaml` and `krab.json` are no longer read.** The orchestrator parses
  `krab.toml` and nothing else; the previous `config`-crate loader accepted
  other extensions incidentally. Rename the file if you had one. The same change
  fixes a defect that was invisible on Windows and total elsewhere: `config`
  lowercased every key it read, so `[services.X].env` entries reached children
  mangled — `RUST_LOG = "info"` arrived as `rust_log`. They now arrive with
  their case intact, so if you worked around this by exporting the variable from
  the parent shell, you can stop.

`port` has no default. A service that declares none still inherits whatever
ambient `KRAB_PORT` exists and logs
`service_port_unpinned_inheriting_ambient_krab_port` when it does, so the
un-migrated case is noisy rather than silent.

#### `krab_core::render_stream`'s streaming writer is not compiled for `wasm32`

**Breaking on `wasm32` only, and only at compile time.** A crate that names
`ChunkedStreamWriter`, `FinishedStream`, `StreamTelemetry` or
`render_to_chunk_stream` in code compiled for `wasm32-unknown-unknown` now
fails to compile; the module itself, and its marker parser, remain. Native
targets are unchanged:
`ChunkedStreamWriter`, `SuspenseState` and streaming SSR behave exactly as they
did.

**The streaming half could never have worked in a browser.**
`ChunkedStreamWriter` times its flushes with a bare `std::time::Instant`, which
compiles on `wasm32-unknown-unknown` and panics on first use — so any call that
actually reached a browser was already a guaranteed runtime panic. Streaming SSR
has no client half ([ADR 0009](../adr/0009-resource-ssr-semantics.md)) and
nothing in `krab_client` consumes the `<!--krab:suspense:*-->` markers.

**The marker parser is not part of the break.** `SuspenseMarker::parse`,
`SuspenseState` and `is_finalized_ssr_snapshot` are pure string parsing — no
`Instant`, no I/O — and they worked on `wasm32` in `0.4.0`. They still do, at the
same `krab_core::render_stream::` paths: the gate is on the writer items inside
the module, not on the module. A browser-side crate that parses
`<!--krab:suspense:*-->` markers needs no change.

What does need a change is a crate that names `ChunkedStreamWriter`,
`FinishedStream`, `StreamTelemetry` or `render_to_chunk_stream` in code compiled
for `wasm32`; that code could only ever have panicked, so gate the import with
`#[cfg(not(target_arch = "wasm32"))]`.

If a shared crate compiles for both targets, gate the use:

```rust
#[cfg(not(target_arch = "wasm32"))]
use krab_core::render_stream::ChunkedStreamWriter;
```

#### `SuspenseMarker` is deprecated

Deprecated in 0.5.0, **removed in 0.6.0**. The replacement is
`krab_core::render_stream::is_finalized_ssr_snapshot`, which takes the rendered
HTML and returns a `bool`. Behaviour is unchanged — it is the same parser behind
a helper that answers the only question anyone was asking of it.

```rust
// Before — parse markers yourself to decide whether a snapshot is cacheable
let finalized = SuspenseMarker::parse(&marker_raw)
    .map(|marker| marker.state == SuspenseState::Resolved)
    .unwrap_or(false);

// After
let finalized = krab_core::render_stream::is_finalized_ssr_snapshot(&html);
```

`SuspenseState` stays public: `ChunkedStreamWriter::write_suspense_marker` takes
it.

#### `ErrorCategory` is `#[non_exhaustive]`

**Breaking for Rust callers only.** A `match` on `krab_core::http::ErrorCategory`
now needs a wildcard arm. Constructing the existing variants is unaffected and
no wire format changes. It is taken in the same release that adds the
`unavailable` category (503, load shedding under `KRAB_HTTP_OVERLOAD_MODE=shed`)
so that every future category is a non-breaking addition.

```rust
match err.category {
    ErrorCategory::RateLimited => back_off(),
    ErrorCategory::Unavailable => fail_over(),   // new in 0.5.0
    _ => surface(err),                           // now required
}
```

#### Not breaking: the per-IP auth-failure limiter became configurable

Read this before tuning anything, because the limiter itself is **not new**.
0.4.0 already answered **429** to a client IP that exceeded 100 auth failures in
a 60-second window, with both numbers hardcoded. 0.5.0 exposes them:

| Variable | Default | Meaning |
| --- | --- | --- |
| `KRAB_AUTH_FAILURE_WINDOW_SECS` | `60` | Window length in seconds. `0` and unparseable values fall back to `60` |
| `KRAB_AUTH_FAILURE_THRESHOLD` | `100` | Auth failures per client IP tolerated within the window. `0` is valid and means the first failure in the window is answered 429 |

The defaults reproduce 0.4.0's behaviour exactly, so **an upgrade that sets
neither variable sees no change in 429 behaviour.** Two properties to size
against if you do change them:

- The window is fixed (tumbling), not sliding — the counter is keyed on
  `floor(unix_secs / window)` and resets at the boundary. The worst case a
  client can spend is `2 x` the threshold across two adjacent windows.
- The comparison is `failures > threshold`, so at the default it is the 101st
  failure in a window that first draws a 429.

Counters are shared across replicas through `DistributedStore` (Redis when
`KRAB_REDIS_URL` is set), and a store error fails closed with 429 rather than
letting the attempt through.

### 0.3.0 — cleaning up orphaned ISR cache keys (Redis-backed ISR only)

The ISR key separator changed from `:` to the control byte ``, so entries
written under the old format are never read again and repopulate automatically
under the new format. TTL'd entries (`Revalidate`, `OnDemand`) age out on their
own. **`Static` entries have no TTL and will linger in Redis unread** until
deleted. This is a memory leak only — never a correctness issue. Skip this
entirely if you do not use `KRAB_REDIS_URL` with ISR.

Old keys have the shape `<namespace>:<path>` and paths always begin with `/`
(default namespace is `krab:isr`). **Preview first:**

```sh
redis-cli --scan --pattern 'krab:isr:/*'
```

When the list looks right, delete in a non-blocking pass:

```sh
redis-cli --scan --pattern 'krab:isr:/*' | xargs -r -L 100 redis-cli del
```

Safety caveats:

- Use `--scan` (SCAN), never `KEYS` — `KEYS` blocks the single Redis thread
  across the whole keyspace.
- The `/` after `krab:isr:` is load-bearing: it matches old colon-separated
  entries (`krab:isr:/blog/x`) while **excluding** new-format sub-namespace keys
  such as `krab:isr:site/blog/x`. Do **not** broaden to `krab:isr:*` —
  that glob would also match live new-format keys of any namespace containing a
  colon.
- For a custom namespace (e.g. `krab:isr:site`), use
  `--pattern 'krab:isr:site:/*'`.
- Deleting old keys is safe at any time, including while serving: the running
  framework only reads ``-separated keys.

### 0.2.0 — `IsrCache` becomes async and store-backed

**Breaking, API.** `IsrCache` kept pages in a process-local `HashMap`, which is
silently wrong the moment you run more than one replica: each holds its own copy
and invalidation reaches only one. It now sits on
`krab_core::store::DistributedStore`.

**What changes at the call site:** every method is `async` and returns
`anyhow::Result`.

```rust
// Before
if let Some(entry) = cache.get("/blog/hello") { … }
cache.put("/blog/hello", html, policy);
let removed = cache.invalidate_prefix("/blog");

// After
if let Some(entry) = cache.get("/blog/hello").await? { … }
cache.put("/blog/hello", html, policy).await?;
let removed = cache.invalidate_prefix("/blog").await?;
```

`IsrEntry::generated_at` is now a `SystemTime` rather than an `Instant`. If you
read it directly, `entry.age()` is unchanged and is the better call.

**To actually get shared caching**, build it over your store instead of
`IsrCache::new()`:

```rust,ignore
let runtime = RuntimeState::try_new()?;             // reads KRAB_REDIS_URL, fails closed outside dev
let isr_cache = IsrCache::with_store(runtime.store.clone());
```

`IsrCache::new()` still works and still means one process only — that is now
documented rather than implied. If you deploy a single instance, no change is
needed beyond the `.await`s.

**If you implement `DistributedStore` yourself**, add `delete`,
`keys_with_prefix`, and (new in 0.3.0) `set_if_absent`. Prefix scans must not
block: use `SCAN`, not `KEYS`.

### 0.2.0 — login credentials become Argon2id hashes

**Breaking, operator-facing.** `KRAB_AUTH_LOGIN_USERS_JSON` and
`KRAB_AUTH_BOOTSTRAP_PASSWORD` held plaintext passwords, compared with `!=`.
They now hold Argon2id hashes in PHC string format, verified with a
constant-time Argon2 verification.

**Who is affected:** any deployment of `service_auth` in an environment other
than `dev`/`local`. Those two still accept a plaintext value and hash it at
startup, so local development needs no change.

**What breaks if you do nothing:** startup fails with a message naming the
offending variable and user. It fails closed — a plaintext credential is never
silently accepted in a non-local environment.

**Migration:**

1. Hash each password:

   ```sh
   krab auth hash-password --username admin
   # reads the password from stdin so it stays out of shell history
   ```

2. Replace the values in your secret store. `KRAB_AUTH_LOGIN_USERS_JSON` keeps
   the same shape — a JSON object keyed by username — with hashes as values:

   ```json
   { "admin": "$argon2id$v=19$m=19456,t=2,p=1$<salt>$<digest>" }
   ```

3. Redeploy. The `*_FILE` and `*_VAULT_REF` sourcing rules are unchanged; only
   the value format differs.

There is no compatibility window on this one. A plaintext password accepted for
one more minor version is a plaintext password in production for one more minor
version, so the deprecation convention in
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) is deliberately not applied here.
See [`docs/reference/security.md`](../reference/security.md#password-credentials).

---

## From Axum

| Axum concept | Krab equivalent |
| --- | --- |
| `Router` and handlers | Keep Axum handlers, then add Krab HTTP layers, service config, and release checks |
| Manual health routes | Standard `/health` and `/ready` routes in generated starters |
| Local process scripts | `krab bootstrap` with supervised startup order and readiness checks |
| Ad hoc CI | `krab doctor` and `krab release certify` evidence bundles |

Recommended path:

1. Move existing routes behind a Krab project model in `krab.toml`.
2. Add `/health` and `/ready`.
3. Apply common HTTP layers and runtime state.
4. Add release certification to CI before changing behavior.

## From Leptos

| Leptos concept | Krab equivalent |
| --- | --- |
| Server functions | `#[server]` functions mounted under `/api/rpc/{name}` |
| Islands | Krab islands with explicit hydration markers |
| SSR policy | `RouteRenderPolicy` with `RenderMode` and `CacheMode` |
| Full-stack app shell | Krab service plus orchestrator and release tooling |

Key difference: Krab treats server functions as public HTTP endpoints and documents validation/auth responsibilities explicitly.

## From Next.js, Astro, SvelteKit, or Nuxt

| Concept | Krab equivalent |
| --- | --- |
| API routes / server actions | `#[server]` functions or Axum handlers |
| Route rendering modes | `RouteRenderPolicy` |
| Islands / partial hydration | Krab island components with server-emitted hydration markers |
| Platform adapters | Rust service deployment plus orchestrator config |
| Preview/deploy checks | `krab doctor` and `krab release certify` |

Krab is not trying to mirror every frontend convention. The trade is Rust-native service composition, explicit operations defaults, and one project model for local development through release evidence.
