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
| `INTERNAL_SERVER_ERROR` | 500 |

## 2. Standard service endpoints

All services expose:

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Liveness check |
| `GET` | `/ready` | Readiness check |
| `GET` | `/metrics` | JSON metrics snapshot |
| `GET` | `/metrics/prometheus` | Prometheus metrics format |

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

## 7. Versioning policy

- REST routes are versioned by path prefix: `/api/v1/...`
- GraphQL versioning is schema-driven
- Breaking changes require migration guidance and release notes in [`CHANGELOG.md`](../../CHANGELOG.md)
