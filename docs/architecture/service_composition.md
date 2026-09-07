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

A readiness probe stops early if the child process exits: a service that dies during startup is reported with its exit status rather than as a probe timeout.

## Restart Policy

`[services.<name>.restart_policy]` governs what happens when a service exits on its own.

| Key | Default | Meaning |
|---|---|---|
| `on_exit` | `true` | Restart the service when it exits |
| `backoff_ms` | `500` | Delay before each restart attempt |
| `max_attempts` | `5` | Restart attempts allowed within one unstable period |
| `stability_window_ms` | `60000` | Uptime after which earlier crashes stop counting against `max_attempts` |

`max_attempts` bounds a crash loop, not a service's lifetime failures. Once a service has stayed up for `stability_window_ms`, its budget resets, so a service that fails once a month is not permanently given up on after `max_attempts` months.

Restarts are scheduled rather than slept through: one service waiting out its backoff does not delay supervision of the others, or the response to Ctrl-C.

## Ordering

Startup order is the topological order of `depends_on` and `startup_dependencies`, resolved deterministically — a cycle or an unknown dependency is a startup error, not a warning.

Shutdown and watch-triggered restarts use the same graph in reverse, so a dependency outlives everything that talks to it and comes back before its dependents do.

## Reference Service

[`services/service_users_split`](../../services/service_users_split/) is the in-tree
worked example: one domain contract behind a REST adapter and a GraphQL adapter in a
single process, registered with the orchestrator on port 3207. It carries the same
runtime governance as every other Krab service — `apply_common_http_layers` over an
`AppState` that implements `HasRuntimeState` — which is what supplies the `AuthContext`
its adapters read. A split service that skips that call has no authenticated identity to
give its adapters, and every API route answers 500 while `/health` and `/ready` stay
green.

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
