# Public Assets

This directory contains static assets served by Axum's `tower_http::services::ServeDir`.

## Building Client

To generate the client-side WASM and JS files, run:

```bash
cd ../../../crates/framework/krab_client
wasm-pack build --target web --out-dir ../../../services/service_frontend/public --no-typescript
```

This will generate `krab_client.js`, `krab_client_bg.wasm`, and `package.json` in this directory.
