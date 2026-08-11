# ADR 0004: Protocol Selection by Explicit Endpoint

## Status

Accepted

## Context

Krab services expose the same domain operations over REST, GraphQL, and RPC.
Something has to decide which adapter serves a given request, and two
incompatible models were specified during design.

The policy layer
(`internal/plans/api_protocol_flexibility_plan.md` §4.5; internal planning
document, not distributed) required protocol to be
chosen by **explicit endpoint**, with runtime override headers removed. The
execution layer (`internal/plans/protocol_flexibility/01_detailed_implementation_blueprint.md`
§3.2; internal planning document, not distributed) made **client-header
selection** implementation goal #1 and ranked client
preference third in a four-step resolver, ahead of the service default.

The two documents contradicted each other for the whole life of the project, and
neither described what was eventually built. This ADR records the decision that
actually governs the code, so the contradiction cannot resurface.

The stakes are not stylistic. Header-driven protocol negotiation means the same
logical operation reaches different adapters depending on a client-supplied
value. Authorization, rate-limit class, error taxonomy, and audit shape are all
per-adapter concerns, so a client that can steer the adapter can steer the
policy surface it is evaluated against. That is a security property, not an
ergonomics one.

## Decision

**The route family is the protocol selector**, over an explicitly enumerated
set of namespaces — not a catch-all:

| Path | Protocol |
|---|---|
| `/api/v1/graphql`, `/api/v1/graphql/*` | GraphQL |
| `/api/v1/rpc`, `/api/v1/rpc/*` | RPC |
| `/api/v1/users`, `/api/v1/users/*` | REST |
| anything else | no route-family match |

Paths outside the table — `/api/v1/auth/*`, `/api/v1/capabilities`, and the ops
routes — deliberately do **not** resolve by route family. They are served by a
single adapter, so there is nothing to select between; resolution falls through
to the service default. Adding a business namespace means adding a row here.

`resolve_protocol_for_request` in
[`krab_core/src/http_protocol.rs`](../../crates/framework/krab_core/src/http_protocol.rs)
resolves in this fixed order:

1. **Compute the allowed set** — enabled protocols, narrowed by
   `restricted_operations` for the operation, then by `tenant_overrides` for the
   request tenant. An empty set rejects.
2. **Route family decides, when it matches.** If the route's protocol is in the
   allowed set, return it. If it is not, reject with `PROTOCOL_NOT_SUPPORTED` —
   never fall through to a different adapter.
3. **Client preference, only when `allow_runtime_switch_header` is true.**
   Default `false`. Reachable only for paths with no route-family match, since
   a match at step 2 always returns or rejects.
4. **Service default.**

Supporting decisions:

- `KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER` gates step 3 and defaults to
  `false`. It is intended for controlled internal gateway experiments.
- `allow_client_override` is **not** a field on `ServiceCapabilities`. Whether
  the switch header is honoured is service configuration, not a client-facing
  capability; advertising it would invite the negotiation this ADR rules out.
- Auth lifecycle operations (`login`, `refresh`, `revoke`, `jwks`) are
  REST-only, enforced through `restricted_operations`, and changing that
  requires a formal security review.
- Capability documents are served at `/api/v1/capabilities` and
  `/api/v1/auth/capabilities` — versioned, matching every other business route.

## Consequences

- A default deployment performs no protocol negotiation. The URL fully
  determines the adapter, so the authorization surface is a function of the
  route, not of client input.
- Step 2 fails closed. Requesting a disabled protocol is an error rather than a
  silent downgrade to an enabled one, so a misconfigured client is visible
  instead of quietly served by an adapter it did not ask for.
- Gateways route on path alone. No body or header inspection is needed to pick
  an upstream, which keeps split-service topology (`users-rest`,
  `users-graphql`, `users-rpc`) a pure routing concern.
- The escape hatch still exists, so an operator can re-enable header switching
  and reintroduce the risk above. It is off by default and should stay off
  outside controlled experiments.
- `krab.selection_source` on each span records which of the four steps decided,
  making an unexpected negotiation observable rather than silent.
- Adding a protocol surface to an operation is a governed API change under
  [`docs/operations/api_governance.md`](../operations/api_governance.md), since
  the route namespace is the contract.
