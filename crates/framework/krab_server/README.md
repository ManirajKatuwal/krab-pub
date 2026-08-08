# krab_server

Hyper/Tower server foundations for the [Krab](https://github.com/krab-framework/krab)
full-stack Rust web framework.

`krab_server` hosts the server-side rendering runtime that serves Krab pages and
the island payloads the browser hydrates. Application services build on top of
it; API services use the shared transport in
[`krab_core`](https://docs.rs/krab_core) instead.

## Usage

```toml
[dependencies]
krab_server = "0.1"
```

## Documentation

- [Architecture deep-dive](https://github.com/krab-framework/krab/blob/main/docs/architecture/design.md)
- [Hydration](https://github.com/krab-framework/krab/blob/main/docs/architecture/hydration.md)
- [Render policy](https://github.com/krab-framework/krab/blob/main/docs/architecture/render_policy.md)

## License

MIT
