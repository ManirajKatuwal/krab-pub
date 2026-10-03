# Split-Service Reference

Use this track when service boundaries and adapter separation matter more than frontend breadth.

## Generate

```bash
krab topology split users --protocols rest,graphql,rpc --register --dry-run
krab topology split users --protocols rest,graphql,rpc --register
krab topology doctor --diagnostics
krab bootstrap
```

## What To Inspect

- the generated split-service crate, `services/service_<domain>_split`
- its `src/domain/` module and its per-protocol `src/adapters/` module
- `krab.toml` service registration
- orchestrator startup dependencies and health checks

## Extension Points

- Move shared request/response types into the domain module.
- Keep transport adapters thin.
- Use topology checks to prevent cross-service imports.
- Swap local adapters for remote service calls once contracts are stable.
