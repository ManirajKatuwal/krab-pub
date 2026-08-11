# krab_macros

Procedural macros for the [Krab](https://github.com/ManirajKatuwal/krab-pub)
full-stack Rust web framework.

| Macro | Purpose |
|---|---|
| `view!` | Builds a `krab_core::Node` tree from markup-like syntax |
| `#[island]` | Marks a component as an interactive island to be hydrated in the browser |
| `#[server]` | Turns an `async fn` into a server function with a generated client stub |

Compile-time misuse is caught by a `trybuild` compile-fail suite covering empty
views, generic islands, mismatched closing tags, and invalid `#[server]`
signatures.

## Usage

```toml
[dependencies]
krab_macros = "0.1"
```

## Documentation

- [Server functions](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/reference/server_functions.md)
- [ADR 0003 — server functions as public endpoints](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0003-server-functions-public-endpoints.md)

## License

MIT
