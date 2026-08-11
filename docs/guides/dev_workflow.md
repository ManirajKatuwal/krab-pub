# Dev Workflow and Build Outputs

## CLI Commands

- `krab build [--release]`
  - Builds `service_frontend`
  - Builds `krab_client` for `wasm32-unknown-unknown`
  - Runs `wasm-bindgen`
  - Produces `dist/assets.json` with fingerprinted assets

- `krab dev [--release]`
  - Runs `krab build`
  - Starts `service_frontend`

## Asset Fingerprinting

The CLI computes a deterministic hash over generated assets:

- `dist/krab_client.js` -> `dist/krab_client.<hash>.js`
- `dist/krab_client_bg.wasm` -> `dist/krab_client_bg.<hash>.wasm`

It also writes:

- `dist/assets.json`

Example:

```json
{
  "krab_client.js": {
    "source": "krab_client.js",
    "fingerprinted": "krab_client.ab12cd34.js"
  },
  "krab_client_bg.wasm": {
    "source": "krab_client_bg.wasm",
    "fingerprinted": "krab_client_bg.ef56gh78.wasm"
  }
}
```

## Notes

- Current `dev` command runs a one-shot build and then launches the server process.
- File watching/HMR can be added by introducing a watcher loop and restarting `service_frontend` on changes.
