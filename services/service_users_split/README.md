# service_users_split

The split-topology reference service: **one domain contract, two protocol
adapters, one process**. It is the shape a `krab topology split <domain>`
service takes before its adapters are moved into separate binaries, and it runs
as a peer of `service_auth`, `service_users`, and `service_frontend` under
`krab bootstrap`.

Port `3207` by default (`KRAB_PORT` overrides; `krab.toml` pins it for the
orchestrator).

## What it demonstrates

- A single `DomainService` trait behind both adapters, so REST and GraphQL
  answer the same question with the same result. `split_runtime_rest_and_graphql_me_are_parity_aligned`
  asserts that parity over real HTTP.
- Transport-specific error mapping kept in the adapters
  (`DomainError` → `ApiError` for REST, → `async_graphql::Error` for GraphQL).
- The same runtime governance every Krab service gets, applied through
  `krab_core::http::apply_common_http_layers`.

## Routes

| Route | Protocol | Auth |
|---|---|---|
| `GET /health` | — | open |
| `GET /ready` | — | open (orchestrator health check) |
| `GET /metrics`, `GET /metrics/prometheus` | — | closed unless `KRAB_METRICS_PUBLIC=true` |
| `GET /api/v1/users/me` | REST | bearer token |
| `POST /api/v1/graphql` | GraphQL | bearer token |

The tenant comes from the verified token's `tenant_id`/`tid` claim, never from
a request header. A token without one is a `400` on REST and a
`tenant context is required` GraphQL error.

## Runtime

`run_default` loads `KrabConfig` from the environment, validates the secrets
policy, resolves protocol exposure, builds `AppState` with the fail-closed
`RuntimeState::try_new`, and serves through
`krab_core::service::serve_with_graceful_shutdown`. Startup returns
`anyhow::Result` throughout — there is no `unwrap`, `expect`, or `panic!` on
the boot path.

Protocol exposure defaults to REST + GraphQL because the framework default is
REST-only, which would answer `/api/v1/graphql` with `PROTOCOL_NOT_SUPPORTED`.
Set `KRAB_PROTOCOL_ENABLED`, `KRAB_PROTOCOL_EXPOSURE_MODE`, or
`KRAB_PROTOCOL_ENABLED_USERS_SPLIT` to override. The service-local key is read
under this service's own name whether or not `KRAB_SERVICE_NAME` is set, so it
works the same run standalone as under `krab bootstrap`. Enabling more than one
protocol also needs `KRAB_PROTOCOL_EXPOSURE_MODE=multi`; single exposure mode
requires exactly one enabled protocol and startup rejects the pair otherwise. `/api/v1/rpc` is deliberately
absent: no RPC adapter is mounted, and protocol resolution rejects the route
family before routing sees it.

The domain is `InMemoryDomainService` on purpose. What this service exists to
show is the adapter boundary; persistence belongs to `service_users`, which
carries the Postgres/SQLite drivers and the migration governance.

## Running it

```sh
cargo run --bin service_users_split          # standalone, :3207
cargo run --bin krab_orchestrator            # with the other three services
cargo test -p service_users_split
```

## Extending it

1. Replace `InMemoryDomainService` with a real implementation of the same
   trait — nothing in the adapters changes.
2. Add an adapter per protocol you enable, and keep transport-specific code
   inside it.
3. Run the same contract conformance tests against every adapter, over real
   HTTP. No test in this crate may inject an `AuthContext` extension: no client
   can, and a suite that does cannot notice when the governance layers stop
   being applied.
