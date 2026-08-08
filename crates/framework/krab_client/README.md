# krab_client

WASM island hydration runtime for the [Krab](https://github.com/krab-framework/krab)
full-stack Rust web framework.

`krab_client` runs in the browser. It locates the hydration markers emitted by
the server render, mounts the islands they describe, and wires up the reactive
signal graph so interactive regions become live without re-rendering the page.

## Building

```sh
wasm-pack build crates/framework/krab_client --release --target web
```

## Usage

```toml
[dependencies]
krab_client = "0.1"
```

## Documentation

- [Hydration](https://github.com/krab-framework/krab/blob/main/docs/architecture/hydration.md)
- [ADR 0001 — hydration markers](https://github.com/krab-framework/krab/blob/main/docs/adr/0001-hydration-markers.md)
- [Signal safety](https://github.com/krab-framework/krab/blob/main/docs/architecture/signal_safety.md)

## License

MIT
