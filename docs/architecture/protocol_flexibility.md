# Protocol Flexibility

This document defines how Krab services expose REST/GraphQL/RPC surfaces, publish capabilities, and resolve protocol selection at runtime.

## 1. Selection model overview

Services publish protocol capabilities and clients resolve calls via explicit endpoint mappings.

Core principles:

- Additive-first rollout
- Environment-driven protocol policy
- Explicit routing (no implicit runtime header switching by default)
- Parity tests required for multi-protocol operations

## 2. Capability endpoint contract

Each protocol-aware service exposes:

- `GET /api/v1/capabilities`

Example payload:

```json
{
  "service": "users",
  "default_protocol": "rest",
  "supported_protocols": ["rest", "graphql", "rpc"],
  "protocol_routes": {
    "rest": "/api/v1/users",
    "graphql": "/api/v1/graphql",
    "rpc": "/api/v1/rpc"
  }
}
```

Auth service additionally exposes:

- `GET /api/v1/auth/capabilities`

and remains REST-only for auth lifecycle operations.

## 3. Client preference signal

Optional client hint header:

- `x-krab-protocol: rest|graphql|rpc`

By default (`KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER=false`), runtime switching is rejected and clients must rely on server policy and route family.

## 4. Resolution priority

Typical order:

1. Explicit route family policy (`/api/v1/graphql`, `/api/v1/rpc`, etc.)
2. Runtime client hint (only when enabled)
3. Service default protocol

Frontend downstream calls resolve protocol from discovered capabilities and operation allowances, with fallback to alternative allowed protocols when the primary call fails.

## 5. Configuration variables

- `KRAB_PROTOCOL_EXPOSURE_MODE=single|multi`
- `KRAB_PROTOCOL_ENABLED=rest,graphql,rpc`
- `KRAB_PROTOCOL_DEFAULT=rest|graphql|rpc`
- `KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER=true|false`
- `KRAB_PROTOCOL_TOPOLOGY=single_service|split_services`
- `KRAB_PROTOCOL_RESTRICTED_OPS_JSON={...}`
- `KRAB_PROTOCOL_TENANT_OVERRIDES_JSON={...}`
- `KRAB_PROTOCOL_SPLIT_TARGETS_JSON={...}`

Frontend-specific:

- `KRAB_PROTOCOL_EXTERNAL_MODE=true|false`
- `KRAB_PROTOCOL_GATEWAY_BASE_URL=http://gateway:8080`
- `KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN=<token>`

## 6. Migration notes for integrators

- Existing REST integrations continue to work with default single-mode settings.
- Multi-protocol adoption should start with read operations that have parity coverage.
- Auth lifecycle endpoints (`login`, `refresh`, `revoke`, `jwks`) remain REST-only and must not be migrated to alternate transports.
- For split topology, configure per-protocol service URLs using `KRAB_PROTOCOL_SPLIT_TARGETS_JSON`.
- Monitor protocol-segmented metrics/traces during rollout and keep rollback switches ready.
