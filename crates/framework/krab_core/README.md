# krab_core

Shared runtime for the [Krab](https://github.com/krab-framework/krab) full-stack
Rust web framework.

`krab_core` holds the pieces every Krab service depends on: configuration and
secret sourcing, the HTTP layer (auth, errors, headers, observability, protocol
negotiation, runtime, security), database and migration governance, telemetry,
resilience primitives, signals, render policy, ISR, i18n, WebSockets, and server
functions.

## Features

`krab_core` ships with **no default features**. Enable what you need:

| Feature | Enables |
|---|---|
| `rest` | Axum-based HTTP layer, JWT auth, tower middleware |
| `graphql` | `async-graphql` integration |
| `grpc-semantics` | gRPC status-code and timeout-header semantics for a gateway (no transport — see note below) |
| `auth` | Argon2id password hashing and the `CredentialStore` trait |
| `db-postgres` | `sqlx` Postgres access and the full migration governance surface |
| `db-sqlite` | `sqlx` SQLite driver (migration governance does not apply) |
| `redis-store` | Redis-backed distributed store |
| `web` | WASM/browser bindings (`web-sys`, `js-sys`, `wasm-bindgen`) |

Deprecated aliases, removable no earlier than `0.3.0`: `grpc` →
`grpc-semantics`, `db` → `db-postgres`.

> **`grpc-semantics` is not gRPC.** It provides the canonical status codes and
> `grpc-timeout` header parsing a **gateway** needs to map between HTTP and gRPC
> — no transport, no codegen, no `.proto` handling, no client. `tonic` and
> `prost` appear nowhere in this workspace. Bring your own transport if you need
> one. The feature was called `grpc` until
> [ADR 0007](https://github.com/krab-framework/krab/blob/main/docs/adr/0007-grpc-feature-disposition.md),
> which is when the name stopped implying a capability the crate does not have.

## Usage

```toml
[dependencies]
krab_core = { version = "0.1", features = ["rest", "db"] }
```

## Documentation

- [Architecture deep-dive](https://github.com/krab-framework/krab/blob/main/docs/architecture/design.md)
- [Environment reference](https://github.com/krab-framework/krab/blob/main/docs/reference/environment.md)
- [Database and migration governance](https://github.com/krab-framework/krab/blob/main/docs/reference/database.md)

## Note on `Node`

`krab_core::Node` is `!Send` — it uses `Rc` via the `Dynamic` variant and holds
`EventListener`. Do not hold a `Node` across an `.await`. See
[signal safety](https://github.com/krab-framework/krab/blob/main/docs/architecture/signal_safety.md).

## License

MIT
