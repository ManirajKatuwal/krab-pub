# Krab API Reference

This document is the public API contract for currently exposed HTTP and GraphQL endpoints.

## 1. Global conventions

- Transport: HTTP/1.1 JSON APIs (except explicitly documented plain-text responses)
- Auth: Bearer token for protected routes
- Correlation: `x-request-id` is accepted and echoed for traceability

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
rejected before protocol resolution (401/429) are labeled `protocol="unknown"`
in metrics and traces.

## 2. Standard service endpoints

All services expose:

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

`GET /ready` returns readiness and dependency state, for example:

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

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | SSR home page with islands hydration |
| `GET` | `/about` | Static page |
| `GET` | `/greet` | Greeting page |
| `GET` | `/blog/{slug}` | Dynamic route |
| `GET` | `/api/status` | Frontend status JSON |
| `GET` | `/rpc/now` | Runtime timestamp + server function version |
| `GET` | `/rpc/version` | Server function version + compatibility policy |
| `GET` | `/data/dashboard` | Dashboard payload |
| `GET` | `/asset-manifest.json` | Asset integrity manifest |
| `POST` | `/api/contact` | Contact form submission endpoint |
| `GET` | `/{locale}` | Locale-prefixed home page. See the i18n route family |
| `GET` | `/robots.txt` | Crawler directives. Uses `KRAB_PUBLIC_BASE_URL` for the sitemap link |
| `GET` | `/sitemap.xml` | Sitemap. URLs are absolute against `KRAB_PUBLIC_BASE_URL` |
| `GET` | `/api/hmr` | Dev-only hot-module-reload channel |

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

Two entry points are exported to JavaScript. A page loads the module, awaits
`init()`, and calls them:

| JS export | Description |
|---|---|
| `hydrate()` | Hydrate every `[data-island]` in the document. Idempotent — an already-hydrated boundary is skipped, not re-bound, so it is safe to call again after inserting markup. |
| `start_router()` | Install the client-side router (see below). Idempotent. |

```html
<script type="module">
  import init, { hydrate, start_router } from '/pkg/krab_client.js';
  await init();
  hydrate();
  start_router();
</script>
```

Rust islands additionally reach two scoped entry points, for markup that arrives
after first paint:

| Rust function | Description |
|---|---|
| `hydrate_within(root: &web_sys::Element)` | Hydrate only the `[data-island]` boundaries inside `root`. |
| `unmount(root: &web_sys::Element)` | Release what hydration created under `root`: event listeners, dynamic regions, and effects. Call it before removing hydrated markup — dropping the DOM node alone leaves all three alive for the life of the page. |

A bundle built without `krab_client`'s `web` feature exports the same names and
does nothing. See [Hydration](../architecture/hydration.md#building-a-bundle-that-hydrates).

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

| Item | Deprecated in | Removed in | Replacement |
|---|---|---|---|
| `krab_core::db::postgres::run_migrations` | 0.5.0 | 0.6.0 | `krab_core::db::postgres::run_versioned_migrations` |
| `krab_client` feature `demo-islands` (`Counter`, `Toggle`, `Likes`) | 0.4.0 | 0.6.0 | Define islands in your own crate with `#[island]` |
| `krab_core::render_stream::SuspenseMarker` | 0.5.0 | 0.6.0 | `krab_core::render_stream::is_finalized_ssr_snapshot` |

`run_migrations` only ever applied one bootstrap migration creating a
`_krab_migrations` table that nothing in the framework reads; the real ledger,
checksums, rollback SQL and failure policy all belong to
`run_versioned_migrations`. Callers should pass their own `&[Migration]` slice
and a `MigrationFailurePolicy`. See
[`database.md`](database.md) for the migration contract.

`SuspenseMarker` only ever parsed the `<!--krab:suspense:{id}:{state}-->`
markers emitted by server-side streaming.
[ADR 0009](../adr/0009-resource-ssr-semantics.md) records streaming as having no
client half, so nothing in the browser consumes those markers and there is no
stable meaning for a downstream crate to build on the parsed form. The one real
use — deciding whether a rendered snapshot has every boundary resolved and is
therefore safe to cache — is now `is_finalized_ssr_snapshot`, which takes the
rendered HTML and returns a `bool`.
