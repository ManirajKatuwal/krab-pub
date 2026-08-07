# Service Composition

Krab treats service composition as part of the framework contract.

## Project Model

`krab.toml` describes both the web project and the local service graph. The `[project]` section drives CLI build/watch behavior. The `[services.*]` sections drive the orchestrator.

Each registered service should define:

- `command` and `args`
- optional `depends_on` and `startup_dependencies`
- `[services.<name>.healthcheck]` targeting `/ready`
- `[services.<name>.restart_policy]`

## Health Semantics

Use `/ready` for startup and dependency readiness. Use `/health` for process liveness.

The orchestrator starts services in dependency order and waits for readiness before moving to dependents. Failed readiness is a failed startup, not a background warning.

## Boundary Semantics

Services should communicate through contracts and adapters, not direct imports from another service crate. `krab topology doctor` checks for direct cross-service imports and validates shared contract payload serialization derives.

## Local-To-Remote Swap

Use the same domain contract for local and remote adapters:

1. Keep request/response payloads in the contract layer.
2. Keep transport-specific code in adapters.
3. Run contract conformance tests against each adapter.
4. Switch orchestrator config from local process execution to remote endpoint configuration when the boundary is stable.

## Commands

```bash
krab topology doctor --diagnostics
krab topology split users --protocols rest,graphql,rpc --register
krab bootstrap
```
