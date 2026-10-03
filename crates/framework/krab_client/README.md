# krab_client

WASM island hydration runtime for the [Krab](https://github.com/ManirajKatuwal/krab-pub)
full-stack Rust web framework.

`krab_client` runs in the browser. It locates the hydration markers emitted by
the server render, mounts the islands they describe, and wires up the reactive
signal graph so interactive regions become live without re-rendering the page.

Beyond hydration it exposes a client-side router: same-origin link clicks swap
the contents of the element marked `data-krab-router-outlet` and re-hydrate,
instead of reloading the document. Every failure path — no outlet, a bad
response, an offline network — falls back to a normal browser navigation.

## Building

`krab_client` defines no islands, so the bundle a page loads is built from
**your** crate — the one whose `#[island]`s it should hydrate — with that
crate's own `web` feature on. The runtime's exports (`hydrate`, `start_router`,
…) come along with it:

```sh
wasm-pack build path/to/my_app_islands --release --target web -- --features web
```

Serve the whole output directory, including its `snippets/` subdirectory: the
per-island panic isolation ships as a JS snippet the glue file imports. The
result belongs on a page that loads it as a module and calls `hydrate()`:

```js
// /app.js, loaded with <script type="module" src="/app.js"></script>
import init, { hydrate, start_router } from '/pkg/my_app_islands.js';
await init();
hydrate();
start_router(); // optional: client-side navigation
```

Keep that bootstrap in a same-origin file, not an inline `<script>`: Krab's
security headers send `script-src 'self' 'wasm-unsafe-eval'`, which blocks
inline scripts.

`hydrate_within(root)` hydrates a subtree that arrived after first paint,
`hydrate_within_selector(selector)` stages hydration by selector and returns the
number of islands that failed, `hydrate_island(element)` hydrates one island
and returns its final state, and `unmount(root)` releases what hydration created under it — call it
before removing hydrated markup, or its listeners, dynamic regions, and effects
stay alive for the life of the page.

## Usage

```toml
[dependencies]
krab_client = "0.6"
```

## Features

| Feature | Default | What it does |
|---|---|---|
| `web` | yes | The hydration runtime, the reconciler, and the router's browser half. **Without it the crate is inert**: `hydrate()` logs one line and returns. |
| `debug` | no | Verbose console tracing of the hydration walk. |

`web` had no default through 0.1, 0.2, and 0.3, so the documented build command — which
did not name it — produced a ~15 KB stub instead of the full runtime (~165 KB
as of 0.6.0), and a
page loading it saw no error, just an island that never came alive. It is a
default as of 0.4.0. Nothing that already passed `--features web` needs to
change.

Take the crate **without** default features when you want the server half of
`#[island]` — the `data-island` / `data-props` wrapper markup that SSR emits and
the browser bundle later hydrates:

```toml
[dependencies]
krab_client = { version = "0.6", default-features = false }
```

## Documentation

- [Hydration](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/architecture/hydration.md)
- [ADR 0001 — hydration markers](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0001-hydration-markers.md)
- [Signal safety](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/architecture/signal_safety.md)

## License

MIT
