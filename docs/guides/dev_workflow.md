# Dev Workflow and Build Outputs

## Project Model

- Frontend bin: `service_frontend`
- Bootstrap bin: `krab_orchestrator`
- Public dir: `services/service_frontend/public`
- Dist dir: `dist`
- Watch roots: ["crates/framework/krab_core/src", "crates/framework/krab_macros/src", "services/service_frontend/src", "crates/framework/krab_client/src", "services/service_frontend/public"]

## CLI Commands

| Command | Description |
|---|---|
| `krab bootstrap [--release]` | Build and run `krab_orchestrator` |
| `krab build [--release]` | Build `service_frontend` plus client/WASM artifacts into `dist` |
| `krab dev --watch [--release] [--poll-ms <n>] [--settle-ms <n>]` | Watch ["crates/framework/krab_core/src", "crates/framework/krab_macros/src", "services/service_frontend/src", "crates/framework/krab_client/src", "services/service_frontend/public"], rebuild changed targets, and restart `service_frontend` |
| `krab dev [--release]` | Build once and run `service_frontend` |
| `krab docs [--out <path>]` | Regenerate this developer workflow document |
| `krab doctor [--diagnostics] [--strict]` | Run aggregated workspace health checks for project model, env policy, service config, and topology |
| `krab release certify [--out <dir>] [--diagnostics] [--json]` | Run release gates and write a structured evidence bundle |
| `krab security dependency-gate [--diagnostics]` | Run local dependency governance gate with cargo-deny (CI parity) |
| `krab topology doctor [--diagnostics]` | Run topology boundary checks (cross-service imports, contract payload derives, endpoint config) |
| `krab topology split <domain> [--protocols rest,graphql,rpc,grpc] [--register] [--dry-run]` | Generate split-service extraction skeleton for a domain with adapter stubs and optional registration |
| `krab watch [--release] [--poll-ms <n>] [--settle-ms <n>]` | Alias dedicated to watch workflow |

## Client/WASM Build

- Client package: `krab_client`
- Crate directory: `crates/framework/krab_client`

`krab build --client --release` runs:

```sh
wasm-pack build --release --target web --out-dir dist -- --features web
```

`#[island]` compiles its hydrating half only under `feature = "web"`. A bundle built without it still loads and still exports `hydrate` — it just does nothing, at roughly a tenth of the size, with no error anywhere. The CLI passes the feature whenever the client crate's manifest declares it.

## Asset Fingerprinting

When a client/WASM package is configured, the CLI fingerprints browser assets and writes `dist/assets.json`.

## Watch/HMR Workflow

`krab dev --watch` (or `krab watch`) performs incremental change detection over the configured watch roots, rebuilds only the necessary targets, mirrors changed public assets, and writes a lightweight HMR signal file at `dist/.hmr_signal`.

## Bootstrap Health Semantics

`krab bootstrap` starts services in dependency order, waits on each startup readiness probe before proceeding, and applies restart policy backoff/attempt limits from `krab.toml`. Use `/ready` for readiness probes and `/health` for liveness checks. Service stdout/stderr are captured with stable `[service::stream]` prefixes and written to `internal/audit/orchestrator/` for artifact collection.
