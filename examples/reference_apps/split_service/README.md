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

- generated domain crate
- generated protocol adapter crates
- `krab.toml` service registration
- orchestrator startup dependencies and health checks

## Extension Points

- Move shared request/response types into the domain crate.
- Keep transport adapters thin.
- Use topology checks to prevent cross-service imports.
- Swap local adapters for remote service calls once contracts are stable.
