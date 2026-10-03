# Public Assets

This directory contains static assets served by Axum's `tower_http::services::ServeDir`.

## Building Client

The frontend's browser bundle is the `service_frontend_islands` crate built
for wasm32 — it holds the islands this service renders, and links
`krab_client`'s hydration runtime. From the repository root:

```bash
wasm-pack build services/service_frontend_islands --release --target web --out-dir ../../dist/pkg -- --features web
```

or `krab build --target client --release`, which reads the same settings from
`krab.toml`. This produces `service_frontend_islands.js` and
`service_frontend_islands_bg.wasm` in `dist/pkg/`, which is where
`KRAB_FRONTEND_PKG_DIR` looks by default.
