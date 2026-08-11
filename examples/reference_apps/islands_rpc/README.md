# Islands + RPC

The reference application for Krab's core pitch: **server-side rendering,
island hydration, and server functions, in one page, from one source file.**

Unlike the other directories under [`examples/reference_apps/`](../), this is a
real crate. It is a workspace member, it compiles in CI, and its tests run on
every push.

## What it demonstrates

| Feature | Where | What to look at |
|---|---|---|
| `view!` for all markup | [`src/lib.rs`](src/lib.rs) `page()` | The whole document, including `data-*` and `aria-*` attributes |
| `#[island]` | `TaskCounter`, `TaskFilter` | Two components with different props structs |
| `#[server]` | `add_task` | Mounted at `/api/rpc/add_task`, called from `TaskCounter`'s click handler |
| Shared source | `src/lib.rs` | The same file builds the server and the browser bundle |

## Run it

```sh
cargo run -p reference_app_islands_rpc --bin islands_rpc_server
# then open http://127.0.0.1:3100
```

The page server-renders without JavaScript. To get the interactive half, build
the browser bundle:

```sh
wasm-pack build examples/reference_apps/islands_rpc --target web -- --features web
```

`--features web` is required. It selects the hydrating half of `#[island]` and
its `inventory` registration; without it the wasm build produces the
server-rendering code path and nothing hydrates.

## Test it

```sh
cargo test -p reference_app_islands_rpc
```

Ten tests, covering the three things this example exists to prove:

- The SSR response carries the hydration markers the client runtime queries on
  (`data-island`, `data-krab-boundary`, `data-krab-boundary-id`,
  `data-krab-boundary-state`, `data-props`).
- `view!` emits hyphenated and namespaced attribute names — the page would
  silently lose its ARIA labels and test hooks if that regressed.
- The `#[server]` endpoint returns the expected payload for a valid request and
  rejects both malformed arguments and invalid values.

## How the two halves are selected

`#[island]` switches on the crate's `web` **feature**; `#[server]` switches on
`target_arch`. That is why the manifest declares native and wasm dependency sets
separately rather than feature-gating one list — `krab_core/rest` pulls axum,
which does not build for `wasm32`.

```
cargo build                          -> server: SSR + /api/rpc handler
wasm-pack build ... --features web   -> browser: hydration + fetch to /api/rpc
```

## Notes

- The server keeps no state. `add_task` validates and echoes, so the tests
  assert a contract rather than a database. Adding persistence is the natural
  next step for anyone using this as a starting point.
- Port `3100` is deliberately outside the `3000`–`3002` range the reference
  services use, so this runs alongside them.
