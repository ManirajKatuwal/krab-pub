# ADR 0010: Protocol Resolution Moves Inside the Auth Boundary

## Status

**Accepted** — 2026-08-12, by the repository owner. Implemented in the same
change (0.3.0).

## Context

`apply_common_http_layers` applied `protocol_resolution_middleware` as the
last `.layer()`, making it the **outermost** middleware — it ran before
`auth_middleware`. Protocol policy supports per-tenant overrides
(`KRAB_PROTOCOL_TENANT_OVERRIDES_JSON`), and the tenant is resolved by
`request_tenant_hint`, which prefers the authenticated `AuthContext` and falls
back to the `x-krab-tenant-id` header or `?tenant_id=`/`?tid=` query.

Because the middleware ran pre-auth, the `AuthContext` branch was dead code on
every request. Tenant identity for protocol policy therefore came exclusively
from a **client-controlled** header or query parameter:

- A client could send another tenant's ID to inherit its (possibly more
  permissive) protocol overrides.
- A client could omit the header entirely to escape its own tenant's
  restrictions and fall back to the service-wide `enabled_protocols`.

A second gap in the same middleware: when a route family mapped to a protocol
that was *not* in `enabled_protocols`, the middleware passed the request
through unhandled instead of rejecting it — a service that mounted a GraphQL
handler but disabled the protocol in config served it anyway.

## Decision

1. Protocol resolution runs **after** authentication. The middleware ordering
   in `apply_common_http_layers` places `protocol_resolution_middleware`
   inside `auth_middleware`, so `AuthContext` is present and the tenant used
   for policy decisions is the one proven by the JWT.
2. The unauthenticated header/query tenant fallback is **disabled by
   default**. It can be re-enabled only via
   `KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED=true`, intended for local development
   without an identity provider. Use of the fallback logs
   `protocol_tenant_hint_untrusted_used` at warn level.
3. A route family whose protocol is not enabled is rejected with the existing
   `PROTOCOL_NOT_SUPPORTED` error instead of passing through.

## Consequences

- Tenant protocol overrides are now a real security boundary: they key off
  authenticated claims only, unless an operator explicitly opts into the
  untrusted hint for development.
- Error precedence changed: a request that is both unauthenticated and
  protocol-invalid now receives the auth error (401) first, where it may
  previously have received the protocol error. This is the correct layering —
  identity before policy — and is pinned by tests.
- Deployments that relied on unauthenticated `x-krab-tenant-id` for tenant
  scoping must either authenticate (recommended) or set the opt-in knob and
  accept that the hint is client-controlled.
