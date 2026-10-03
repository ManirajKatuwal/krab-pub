# Krab API Reference

This document is the public API contract for currently exposed HTTP and GraphQL endpoints.

## 1. Global conventions

- Transport: HTTP/1.1 JSON APIs (except explicitly documented plain-text responses)
- Auth: Bearer token for protected routes
- Correlation: `x-request-id` is accepted and echoed for traceability when it is
  1–128 bytes of `[A-Za-z0-9._:-]`; any other value is replaced with a generated
  id (0.6.0)

### Error shape

```json
{
  "code": "UNAUTHORIZED",
  "message": "Bearer token missing or invalid",
  "request_id": "01HV...",
  "trace_id": "01HV..."
}
```

| Error code | HTTP status |
|---|---|
| `UNAUTHORIZED` | 401 |
| `FORBIDDEN` | 403 |
| `NOT_FOUND` | 404 |
| `BAD_REQUEST` / `VALIDATION_ERROR` | 400 |
| `CONFLICT` | 409 |
| `TOO_MANY_REQUESTS` | 429 |
| `PROTOCOL_NOT_SUPPORTED` | 400 |
| `SERVICE_OVERLOADED` | 503 |
| `INTERNAL_SERVER_ERROR` | 500 |

The error envelope also carries a machine-readable `category`. As of **0.3.0**
the category set gained `rate_limited` (429) and `unauthenticated` (401), and
HTTP status derives from the category alone rather than from special-cased code
strings. Two consequences for clients on the 0.2.x → 0.3.0 upgrade:

- An `ApiError` built with `category=authz` and `code=UNAUTHORIZED` now maps to
  **403**; use the `unauthenticated` category (401) for authentication failures.
- A `code=TOO_MANY_REQUESTS` under any category other than `rate_limited` no
  longer forces **429**; rate limiting now uses the `rate_limited` category.

As of **0.5.0** `ErrorCategory` is `#[non_exhaustive]`. A Rust client that
matches on it needs a wildcard arm; in exchange, every future category is a
non-breaking addition. This is a one-time cost taken in the same release that
adds `unavailable`, rather than a cost repeated on each new category.

As of **0.5.0** the category set gains `unavailable` (503), used for
`SERVICE_OVERLOADED` when `KRAB_HTTP_OVERLOAD_MODE=shed` drops a request the
service has no capacity for. It is deliberately distinct from `rate_limited`
(429), which says the *caller* asked for too much; load balancer and alerting
policies that retry or fail over on 503 need the difference. Clients that match
exhaustively on `category` gain one variant.

A request to a route family whose protocol is disabled by configuration returns
**400 `PROTOCOL_NOT_SUPPORTED`** (previously it reached the handler). Requests
rejected before protocol resolution (401/429/503) are labeled
`protocol="unknown"` in metrics and traces.

### Authentication responses

The authentication layer answers with a bare status and **no JSON body**: `401`
(missing or invalid credentials), `403` (admin path without the admin
entitlement), `429` (the client address is over its auth-failure budget —
`KRAB_AUTH_FAILURE_THRESHOLD` failures per `KRAB_AUTH_FAILURE_WINDOW_SECS`),
`503` (a JWKS that has not loaded, or the revocation or limiter store
unreachable) and `500` (misconfiguration, such as malformed policy JSON). Since
0.6.0 only 401-class failures count against the budget, a limiter-store outage
answers 503 rather than 429, and a token **without** a `kid` from an address
already over budget is answered 429 before verification (tokens naming a `kid`
are always verified). The envelope's `request_id` / `trace_id` fields are not
populated by `krab_core`.

## 2. Standard service endpoints

`service_auth`, `service_users`, `service_users_split` and `service_frontend`
expose all four below. (`service_frontend` gained its two metrics routes in
0.6.0; before that `GET /metrics` there fell through to `/{locale}`, and the
`krab_frontend` job in `monitoring/prometheus.yml` collected nothing.)

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Liveness check |
| `GET` | `/ready` | Readiness check |
| `GET` | `/metrics` | JSON metrics snapshot — **requires auth** unless `KRAB_METRICS_PUBLIC=true` |
| `GET` | `/metrics/prometheus` | Prometheus metrics format — **requires auth** unless `KRAB_METRICS_PUBLIC=true` |

As of **0.5.0** the two metrics endpoints are no longer anonymous by default: a
route inventory with per-route volumes, error counts and latency histograms is
reconnaissance, and on a low-traffic service enough to infer individual user
activity. Scrapers must authenticate, or the operator opts back in with
`KRAB_METRICS_PUBLIC=true` (see
[`environment.md`](environment.md)). `/health` and `/ready` are unchanged and
remain anonymous.

### Service identity and ports

The base URLs in the sections below are declared, not ambient. As of **0.5.0**
the orchestrator owns each service's identity: `[services.X].port` and
`[services.X].service_name` in `krab.toml` are injected into the child as
`KRAB_PORT` and `KRAB_SERVICE_NAME` at spawn, above the inherited environment
and below explicit `[services.X].env` entries. Previously an ambient `KRAB_PORT`
moved every service onto one port, and the failure surfaced as a readiness-probe
timeout rather than a bind error. See
[ADR 0012](../adr/0012-orchestrator-owns-service-identity.md).

Three consequences for a deployment upgrading from 0.4.0:

- `service_name` defaults to the `[services.<key>]` table key, so the `service`
  label on log lines and on the metrics this document exposes follows the
  manifest key unless the field is declared.
- Two services declaring the same `port`, two resolving to the same
  `service_name`, or a `port` outside 1-65535 are startup errors, reported by
  name before anything is spawned. `krab topology doctor` reports the same
  statically.
- The orchestrator reads `krab.toml` only; `krab.yaml` and `krab.json` were
  previously accepted incidentally.

`GET /ready` on `service_auth` and `service_users` returns readiness and
dependency state, for example (`service_frontend` and `service_users_split`
answer a static `ready` without checking dependencies):

```json
{
  "status": "ready",
  "uptime_seconds": 42,
  "dependencies": [
    {
      "name": "postgres",
      "ready": true,
      "critical": true,
      "latency_ms": null,
      "detail": "connection-pool-available"
    }
  ]
}
```

## 3. Auth service (`service_auth`)

Base URL: `http://localhost:3001`

### `POST /api/v1/auth/login`

Issues access + refresh tokens.

Request:

```json
{
  "username": "admin",
  "password": "<password>",
  "tenant_id": "tenant-a",
  "scopes": ["user", "admin"],
  "roles": ["admin"]
}
```

Success response:

```json
{
  "token_type": "Bearer",
  "access_token": "...",
  "refresh_token": "...",
  "expires_in": 900,
  "refresh_expires_in": 604800,
  "kid": "default"
}
```

### `POST /api/v1/auth/refresh`

Rotates and reissues token pair.

```json
{ "refresh_token": "..." }
```

### `POST /api/v1/auth/revoke`

Revokes token.

```json
{ "token": "..." }
```

### `GET /api/v1/auth/jwks`

Returns active signing key descriptors.

### `GET /api/v1/auth/status`

Returns auth subsystem status metadata.

### `GET /api/v1/auth/capabilities`

Returns auth protocol capability metadata.

Auth capability policy is intentionally REST-only for lifecycle operations.

### `GET /api/v1/private`

Protected test/private endpoint.

Success body (plain text):

```text
private_ok
```

## 4. Users service (`service_users`)

Base URL: `http://localhost:3002`

### `POST /api/v1/graphql`

Protected GraphQL endpoint.

Current query contract:

```graphql
type Query {
  me: User!
}

type User {
  id: String!
  username: String!
}
```

Example request:

```json
{ "query": "{ me { id username } }" }
```

### `GET /api/v1/admin/audit`

Protected admin endpoint (admin scope/role required).

### `GET /api/v1/users/me`

Protected users endpoint.

### `GET /api/v1/capabilities`

Returns users service capability metadata for protocol-aware clients.

### `POST /api/v1/rpc`

RPC protocol surface for the users domain, mounted when `rpc` is in
`KRAB_PROTOCOL_ENABLED`. Carries the same operations as the REST and GraphQL
surfaces; the envelope is the shared RPC wire format. Internally the users
service adapter mounts this at `/rpc`; `/api/v1/rpc` is the published path
advertised by the capability endpoint.

## 5. Protocol capability discovery and selection

Protocol-aware services expose capability endpoints to publish default protocol,
supported protocol set, and protocol routes.

Client hint header:

- `x-krab-protocol: rest|graphql|rpc`

Runtime header switching is disabled by default and must be explicitly enabled.

Auth lifecycle restrictions:

- `auth.login`, `auth.refresh`, `auth.revoke`, `auth.jwks`, `auth.status` are REST-only.

## 6. Frontend service (`service_frontend`)

Base URL: `http://localhost:3000`

**Authentication.** The routes `service_frontend` declares public in code
(`RuntimeState::with_public_paths`) are `/`, `/contact`, `/api/contact`,
`/api/status`, `/data/dashboard`, `/rpc/version`, `/rpc/now`,
`/asset-manifest.json`, `/robots.txt`, `/sitemap.xml`, `/blog/*`, `/pkg/*`,
`/streaming`, `/_krab/stream.js`, `/_krab/home.js` and `/_krab/contact.js`.
Every other route below — including `/about`, `/greet`, `/{locale}`,
`/metrics`, `/metrics/prometheus`, `/api/hmr` and the WebSocket endpoints —
requires a bearer token unless listed in `KRAB_AUTH_PUBLIC_PATHS` (or, for the
two metrics routes, `KRAB_METRICS_PUBLIC=true`). An explicit
`KRAB_AUTH_OPEN_PATHS` replaces the code-declared list entirely (see
[security.md](security.md#authentication-model)).

`/robots.txt` and `/sitemap.xml` became public in 0.6.0; before that every
crawler, which carries no token, got `401`. `/about`, `/greet` and
`/{locale}` are deliberately left authenticated — whether they are public is
an application decision — even though the sitemap lists `/about` and
`/greet`: an anonymous crawler following those two entries gets `401`. Add
them to `KRAB_AUTH_PUBLIC_PATHS` if your deployment wants them indexed.

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | SSR home page with islands hydration |
| `GET` | `/contact` | Contact page (generated from `src/routes/contact.rs`). No inline script or `on*=` handler attribute; the form is wired by `/_krab/contact.js` |
| `GET` | `/about` | Static page |
| `GET` | `/greet` | Greeting page |
| `GET` | `/blog/{slug}` | Dynamic route |
| `GET` | `/api/status` | Frontend status JSON |
| `GET` | `/metrics` | JSON metrics snapshot ([§2](#2-standard-service-endpoints)) — **requires auth** unless `KRAB_METRICS_PUBLIC=true` (added in 0.6.0) |
| `GET` | `/metrics/prometheus` | Prometheus metrics ([§2](#2-standard-service-endpoints)) — **requires auth** unless `KRAB_METRICS_PUBLIC=true` (added in 0.6.0) |
| `GET` | `/rpc/now` | Runtime timestamp + server function version |
| `GET` | `/rpc/version` | Server function version + compatibility policy |
| `GET` | `/data/dashboard` | Dashboard payload |
| `GET` | `/asset-manifest.json` | Asset integrity manifest |
| `GET` | `/_krab/home.js` | The home page's hydration runtime (external so the CSP's `script-src 'self'` allows it; configured by the page's `krab-home-config` JSON block) |
| `GET` | `/_krab/contact.js` | The contact form's submit handler, attached with `addEventListener`; posts JSON to `/api/contact`. `application/javascript`. Before 0.6.0 this was an inline `<script>` and an `onsubmit=` attribute, both blocked by Krab's CSP, so the form never submitted |
| `POST` | `/api/contact` | Contact form submission endpoint |
| `GET`, `POST` | `/api/hello` | Example API module (`src/api/hello.rs`) |
| `GET` | `/api/middleware_probe` | Example API module (`src/api/middleware_probe.rs`) |
| `GET` | `/api/users/{id}` | **Authenticated.** A user through the users-contract adapter the topology selected — in-process in a single topology, `service_users` over REST in a distributed one. `200` with `{id, email, display_name}`; contract errors map to `400`/`401`/`403`/`404`/`409`/`502`/`504`/`500` with `{code, message}` (added in 0.6.0). The in-process adapter is a stub: it synthesises `{id, "<id>@local.krab", "local_<id>"}` for any id without a lookup. The remote adapter supports only `id=me` (other ids → `400 users.remote.unsupported_lookup`), forwarding the caller's `Authorization`; an upstream 401 or 403 becomes `401` |
| `POST` | `/api/users` | **Authenticated.** Create a user through the same adapter; body `{email, display_name}`, `201` on success (added in 0.6.0). The in-process adapter echoes a record and stores nothing; the remote adapter always answers `403 users.remote.read_only` |
| `GET` | `/{locale}` | Locale-prefixed home page. See the i18n route family |
| `GET` | `/robots.txt` | Crawler directives. Uses `KRAB_PUBLIC_BASE_URL` for the sitemap link |
| `GET` | `/sitemap.xml` | Sitemap. URLs are absolute against `KRAB_PUBLIC_BASE_URL` |
| `GET` | `/api/hmr` | Hot-reload SSE channel. Registered in every environment; it only emits in `dev` |
| `GET` | `/streaming` | Progressive streaming SSR demo: the shell and a `<Suspense>` fallback flush first, the slow report streams in later ([ADR 0017](../adr/0017-progressive-streaming-ssr.md)). `Cache-Control: no-store`; no render policy (added in 0.6.0) |
| `GET` | `/_krab/stream.js` | The streaming swap runtime (`krab_core::render_stream::STREAM_SWAP_SCRIPT`) a streamed page loads; `application/javascript` (added in 0.6.0) |

### WebSocket endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/ws/chat` | WebSocket upgrade for the chat stream |
| `POST` | `/api/ws/publish` | Publish a message to connected `/api/ws/chat` subscribers |

`/api/ws/chat` is an upgrade endpoint, not a JSON route — it does not return the
standard error envelope. Connection failures surface as HTTP status codes on the
upgrade handshake.

Example `POST /api/contact` request:

```json
{
  "name": "Jane Doe",
  "email": "jane@example.com",
  "message": "Need enterprise onboarding support"
}
```

Accepted response:

```json
{
  "status": "accepted",
  "queued": true,
  "contact": {
    "name": "Jane Doe",
    "email": "jane@example.com"
  }
}
```

## 7. Browser runtime (`krab_client`)

These entry points are exported to JavaScript, and are public Rust functions of
the same name. A page loads the module, awaits `init()`, and calls them:

| JS export | Description |
|---|---|
| `hydrate()` | Hydrate every `[data-island]` in the document. Idempotent — an already-hydrated boundary is skipped, not re-bound, so it is safe to call again after inserting markup. |
| `hydrate_within(root)` | Hydrate `root` if it is an island, and every `[data-island]` inside it. Exported to JS since 0.6.0. |
| `hydrate_within_selector(selector)` | Hydrate every document match of `selector` and the islands inside each; returns the number of boundaries that failed (panicked) in the call. For staged hydration. New in 0.6.0. |
| `hydrate_island(element)` | Hydrate `element` alone; returns the boundary state it ended in (`ok`, `patched`, `decode-error`, `missing-definition`, `error`), or `undefined` if `element` is not an island. New in 0.6.0. |
| `unmount(root)` | Release what hydration created under `root`: event listeners, dynamic regions, and effects. Call it before removing hydrated markup — dropping the DOM node alone leaves all three alive for the life of the page. Exported to JS since 0.6.0. |
| `start_router()` | Install the client-side router (see below). Idempotent. |

The module is the wasm-pack bundle of **your** islands crate, which re-exports
these functions: `krab_client` itself defines no islands, so its own bundle
hydrates nothing. The reference frontend loads
`/pkg/service_frontend_islands.js`.

```html
<script type="module">
  import init, { hydrate, start_router } from '/pkg/my_app_islands.js';
  await init();
  hydrate();
  start_router();
</script>
```

Every hydration entry point is isolated per island: an island that panics is
stamped `data-krab-boundary-state="error"` and given a `role="alert"` fallback,
and the rest still hydrate. None of them throws for it. The isolation ships as a
JS snippet under the bundle's `snippets/` directory, which must be served beside
the glue file. See [Panic isolation](../architecture/hydration.md#panic-isolation).

A bundle built without `krab_client`'s `web` feature exports only `hydrate()`,
`start()`, and nothing that touches the DOM; its `hydrate()` does nothing. See
[Hydration](../architecture/hydration.md#building-a-bundle-that-hydrates).

As of **0.5.0** the streaming writer in `krab_core::render_stream` is not compiled
for `wasm32`. A crate that names it in code built for `wasm32-unknown-unknown`
no longer compiles;
native targets are unchanged.

The streaming half could not have worked there: at 0.5.0 streaming SSR had no
client half ([ADR 0009](../adr/0009-resource-ssr-semantics.md); 0.6.0 adds one
for progressive streaming, see section 9), and `ChunkedStreamWriter` timed
its flushes with a bare `std::time::Instant`, which compiles on that target and
panics on first use — so any browser call into it was already a guaranteed
runtime panic.

The gate is on the writer, not the module. `SuspenseState` and
`is_finalized_ssr_snapshot` are pure string parsing, worked on `wasm32` in
`0.4.0`, and remain available there at the same paths. (`SuspenseMarker`, which
`is_finalized_ssr_snapshot` replaced, was removed in 0.6.0.)
What is gone from `wasm32` is `ChunkedStreamWriter`, `FinishedStream`,
`StreamTelemetry` and `render_to_chunk_stream`. Callers sharing a crate across
both targets gate those imports with `#[cfg(not(target_arch = "wasm32"))]`.

### Client-side navigation requests

When `start_router()` handles a link click or a Back/Forward, it fetches the
destination with:

| Header | Value | Meaning |
|---|---|---|
| `x-krab-router` | `1` | The request is an in-app navigation, not a document load. The response is parsed for `data-krab-router-outlet` and only that element's contents are used. |
| `accept` | `text/html` | The router swaps HTML. |

The header is advisory and the server is free to ignore it — a plain full-page
response is handled correctly. A server that does observe it may use it to log
navigations separately, or to return a lighter shell. Whatever it returns must
still contain a `data-krab-router-outlet` element; without one the router falls
back to a full browser navigation.

Responses to router fetches are subject to the same auth, rate-limit, and error
contract as any other `GET` on the route.

## 8. Versioning policy

- REST routes are versioned by path prefix: `/api/v1/...`
- GraphQL versioning is schema-driven
- Breaking changes require migration guidance and release notes in [`CHANGELOG.md`](../../CHANGELOG.md)

### Rust API deprecations

Every row below is **removed** as of 0.6.0; the migration guide's
[0.5.0 → 0.6.0](../guides/migration_guide.md#050--060) section has the
replacement for each.

| Item | Deprecated in | Removed in | Replacement |
|---|---|---|---|
| `krab_core::db::postgres::run_migrations` | 0.5.0 | 0.6.0 | `krab_core::db::postgres::run_versioned_migrations` |
| `krab_client` feature `demo-islands` (`Counter`, `Toggle`, `Likes`) | 0.4.0 | 0.6.0 | Define islands in your own crate with `#[island]` |
| `krab_core::render_stream::SuspenseMarker` | 0.5.0 | 0.6.0 | `krab_core::render_stream::is_finalized_ssr_snapshot` |
| `krab_core` features `db`, `grpc`; module alias `krab_core::grpc` | 0.2.0 | 0.6.0 | `db-postgres`, `grpc-semantics`, `krab_core::grpc_semantics` |
| `krab_core::config::KrabConfig::from_env` | 0.3.0 | 0.6.0 | `KrabConfig::from_env_checked` |
| `krab_core::ws::WsRoom::connect` / `disconnect` | 0.3.0 | 0.6.0 | `WsRoom::join` (drop the guard to disconnect) |

Deprecated in 0.6.0, removed in 0.7.0:

| Item | Replacement |
|---|---|
| `krab_core::telemetry::init_tracing(name)` — logs `krab_core`'s version as the service's | `init_tracing_with_version(name, env!("CARGO_PKG_VERSION"))` |
| Prometheus histogram `krab_request_duration_seconds_bucket` | `krab_http_request_duration_seconds_bucket` (plus `_sum`, `_count`) |
| `krab_core::image` (`optimized_image`, `ImageProps`) | A `<picture>` written with `view!` against variants your pipeline produces |
| `krab_core::style_scope` | None in the framework; use your CSS tooling's scoping |
| `krab_core::telemetry::{RequestTelemetry, RedMetrics, EndpointMetrics}` | The HTTP layers' request ids and `RuntimeState` metrics |
| Default open-path application routes (`KRAB_AUTH_LEGACY_OPEN_PATHS`) | `KRAB_AUTH_PUBLIC_PATHS` / `RuntimeState::with_public_paths` |
| `internal/audit/` artifact fallback | `KRAB_ARTIFACT_DIR` (default `.krab`) |

`run_migrations` only ever applied one bootstrap migration creating a
`_krab_migrations` table that nothing in the framework reads; the real ledger,
checksums, rollback SQL and failure policy all belong to
`run_versioned_migrations`. Callers should pass their own `&[Migration]` slice
and a `MigrationFailurePolicy`. See
[`database.md`](database.md) for the migration contract.

`SuspenseMarker` only ever parsed the `<!--krab:suspense:{id}:{state}-->`
markers emitted by server-side streaming.
At 0.5.0, [ADR 0009](../adr/0009-resource-ssr-semantics.md) recorded streaming
as having no client half, so nothing in the browser consumed those markers and
there was no stable meaning for a downstream crate to build on the parsed form.
(0.6.0's `<Suspense>` hydration and progressive streaming do consume
`krab:suspense` markers, through `krab_core::suspense` and `krab_client`, not
through this parser.) The one real
use — deciding whether a rendered snapshot has every boundary resolved and is
therefore safe to cache — is now `is_finalized_ssr_snapshot`, which takes the
rendered HTML and returns a `bool`.

## 9. Rendering APIs (`view!` components and context)

### Component tags in `view!`

Since **0.6.0** a capitalised tag in `view!` calls a component function instead
of being a compile error ([ADR 0013](../adr/0013-view-component-tags.md),
superseding [ADR 0006](../adr/0006-view-component-composition.md)):

| Syntax | Expands to |
|---|---|
| `<Card title="x" count={n}/>` | `Card(CardProps { title: Into::into("x"), count: n })` |
| `<ui::Card>...</ui::Card>` | `ui::Card(ui::CardProps { children: <content as one Node> })` |
| `<Button label="Go" ../>` | `Button(ButtonProps { label: Into::into("Go"), ..Default::default() })` |

The call runs inside `krab_core::signal::with_owner`, giving each component its
own context scope. A component is `fn Name(props: NameProps) -> krab_core::Node`
— the `#[island]` signature, so islands work as tags. `Show` and `For` remain
reserved. `on:` listeners and namespaced attributes on a component are compile
errors. No existing call site changes: every such tag failed to compile before.

`krab_core::Node` implements `Default` (an empty fragment), so a props struct
with a `children: Node` field can `#[derive(Default)]`.

### Context (`krab_core::signal`)

| Item | Signature | Description |
|---|---|---|
| `provide_context` | `fn provide_context<T: Clone + 'static>(value: T)` | Store `value` in the current owner. Replaces an earlier `T` in the same owner; shadows an outer one. Outside any owner: no-op plus a `context_provided_without_owner` warning. |
| `use_context` | `fn use_context<T: Clone + 'static>() -> Option<T>` | The nearest `T` provided in the current owner or an ancestor, cloned. |
| `with_owner` | `fn with_owner<T>(f: impl FnOnce() -> T) -> T` | Run `f` in a new owner parented to the current one. The SSR entry point for a request's contexts. |
| `Owner::new` | `fn new() -> Owner` | A new owner, parented to the current one (or a root). Not made current. |
| `Owner::current` | `fn current() -> Option<Owner>` | The owner code is running under. |
| `Owner::with` | `fn with<T>(&self, f: impl FnOnce() -> T) -> T` | Run `f` with this owner current; restored on return or unwind. |
| `Owner::dispose` | `fn dispose(&self)` | Drop this owner's contexts; later provides into it are ignored and lookups skip it. |

Effects and memos run under an owner created with them, so a re-run sees the
contexts that surrounded its creation; what an effect's body provides is
cleared before its next run and dropped on disposal. Owners are thread-local
and `!Send`. See [ADR 0014](../adr/0014-context-api-and-owners.md).

### Reactive attributes

Since **0.6.0** an attribute whose value is a closure literal is reactive
([ADR 0015](../adr/0015-reactive-attributes.md)):
`<button disabled={move || busy.get()}>` expands to
`krab_core::Attribute::dynamic("disabled", move || busy.get())`. The server
renders the value at render time; in the browser an effect patches the
attribute (and the live `value` / `checked` / `selected` property of form
controls) whenever the signals the closure reads change.

| Item | Signature | Description |
|---|---|---|
| `Attribute::new` | `fn new(name: String, value: String) -> Attribute` | A static attribute. |
| `Attribute::dynamic` | `fn dynamic<F: Fn() -> V + 'static, V: IntoAttributeValue>(name: impl Into<String>, source: F) -> Attribute` | A reactive attribute; `None` from the source omits it. |
| `Attribute::is_dynamic` | `fn is_dynamic(&self) -> bool` | Whether `dynamic` is set. |
| `Attribute::current_value` | `fn current_value(&self) -> Option<String>` | `value`, or the source's current result. |
| `IntoAttributeValue` | `fn into_attribute_value(self) -> Option<String>` | Implemented for strings, numbers, `char`, `bool` (`true` present/empty, `false` absent) and `Option<T>`. |
| `DynamicAttributeValue` | `type DynamicAttributeValue = Rc<dyn Fn() -> Option<String>>` | The stored source. |

**Breaking:** `Attribute` has a new public field, `dynamic`, so a struct literal
`Attribute { name, value }` no longer compiles. Use `Attribute::new(name, value)`
(which existed before 0.6.0) or add `dynamic: None`.

### `<script>` and `<style>` content

Since **0.6.0** the text children of `<script>` and `<style>` render verbatim
instead of HTML-escaped — a browser does not decode entities inside these
raw-text elements, so escaping broke every `=>` and `&&`. The sequences that
would end the element early are rewritten: `</script` and `</style` (any case)
become `<\/script` / `<\/style`, and `<!--` inside a script becomes
`\u003C!--`. A page that worked around the old escaping by splicing script text
in after rendering can emit it as an ordinary text child.

### `<Suspense>` (`krab_core::suspense`)

Since **0.6.0** `<Suspense fallback={|| view! { ... }}>children</Suspense>` is a
built-in control-flow tag ([ADR 0016](../adr/0016-suspense-boundaries.md));
`Suspense` is reserved like `Show` and `For`, and `fallback` is required.
Resources created while its children are built register with it, and it shows
`fallback` until each has produced a first value (a refetch or failure does not
bring the fallback back). The server renders it synchronously — children with
initial data, fallback without — between `<!--krab:suspense:{id}:pending-->` and
`<!--krab:suspense:{id}:resolved-->`.

| Item | Signature | Description |
|---|---|---|
| `suspense` | `fn suspense<F: Fn() -> Node + 'static, C: FnOnce() -> Node>(fallback: F, children: C) -> Node` | What the tag expands to. |
| `use_suspense` | `fn use_suspense() -> Option<SuspenseContext>` | The nearest enclosing boundary. |
| `SuspenseContext::register` | `fn register(&self, is_pending: impl Fn() -> bool + 'static) -> SourceHandle` | Hold the boundary on its fallback while `is_pending` is true (a tracked read). |
| `SuspenseContext::is_pending` / `id` / `source_count` | | Inspection. |
| `SourceHandle::mark_streamable` | `fn mark_streamable(&self)` | Tell the boundary a streaming render can resolve this source (used by `with_server_loader`). |

**Breaking:** `krab_core::Node` has a new variant, `Node::Comment(String)`,
rendered as `<!--text-->`. A `match` that named every variant needs an arm for
it.

### Progressive streaming (`krab_core::render_stream`, feature `rest`)

Since **0.6.0** ([ADR 0017](../adr/0017-progressive-streaming-ssr.md)):

| Item | Signature | Description |
|---|---|---|
| `render_to_stream` | `fn render_to_stream<F: FnOnce() -> Node + Send + 'static>(render: F) -> RenderStream` | Render on a dedicated thread; flush the shell with deferred `<Suspense>` fallbacks; stream each resolved boundary. Serve with `Body::from_stream`. |
| `render_to_stream_with` | `fn render_to_stream_with<F>(options: StreamOptions, render: F) -> RenderStream` | With options. |
| `StreamOptions` | `{ timeout: Duration, swap_script_src: String, doctype: bool }` | Defaults: 10 s, `/_krab/stream.js`, `true`. |
| `RenderStream` | `impl Stream<Item = Result<Bytes, Infallible>> + Send` | The response body. |
| `STREAM_SWAP_SCRIPT` / `STREAM_SWAP_SCRIPT_PATH` | `&str` | The swap runtime the application serves (an external script, because Krab's CSP blocks inline ones) and its default path. |
| `Resource::with_server_loader` | `fn with_server_loader<L: FnOnce() -> Fut, Fut: Future<Output = Result<T, E>> + Send + 'static, E: Display>(self, loader: L) -> Self` where `T: Send` | How the server loads the resource during a streaming render. Never called outside one. |

`krab_client::hydrate()` now also listens for the two events the swap runtime
dispatches on `document`, each with `detail: { id, nodes }`:
`krab:suspense-resolving` (the fallback nodes, just before removal — their
islands are unmounted) and `krab:suspense-resolved` (the swapped-in nodes —
their islands are hydrated).

`service_frontend` serves the demo at `GET /streaming` and the swap runtime at
`GET /_krab/stream.js` (both public).
