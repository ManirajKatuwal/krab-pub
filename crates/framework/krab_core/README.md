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
| `grpc` | gRPC status and metadata semantics (no transport — see note below) |
| `db` | `sqlx` database access and migration governance |
| `redis-store` | Redis-backed distributed store |
| `web` | WASM/browser bindings (`web-sys`, `js-sys`, `wasm-bindgen`) |

> The `grpc` feature provides gRPC **status code and metadata semantics** for
> protocol negotiation. It does not bundle a gRPC transport; bring your own
> (`tonic`, etc.) if you need one.

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
