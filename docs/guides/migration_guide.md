# Migration Guide

This guide maps common framework concepts to Krab equivalents. It is intentionally conceptual; use the reference apps for concrete layouts.

---

## Upgrading within Krab

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

#### `krab_core::render_stream` is not compiled for `wasm32`

**Breaking on `wasm32` only, and only at compile time.** A crate that names
`krab_core::render_stream` in code compiled for `wasm32-unknown-unknown` now
fails to compile with an unresolved-module error. Native targets are unchanged:
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
