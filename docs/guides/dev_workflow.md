# Dev Workflow and Build Outputs

## Project Model

- Frontend bin: `service_frontend`
- Bootstrap bin: `krab_orchestrator`
- Public dir: `services/service_frontend/public`
- Dist dir: `dist`
- Watch roots: ["crates/framework/krab_core/src", "crates/framework/krab_macros/src", "services/service_frontend/src", "services/service_frontend_islands/src", "crates/framework/krab_client/src", "services/service_frontend/public"]

## CLI Commands

`--diagnostics` and `--json` are global: they may be given before or after the subcommand, and are listed below only on the commands that act on them. `--json` changes what is printed, never the exit status.

| Command | Description |
|---|---|
| `krab bootstrap [--release]` | Build and run `krab_orchestrator` |
| `krab build [--release]` | Build `service_frontend` plus client/WASM artifacts into `dist` |
| `krab completions <shell>  (bash, zsh, fish, powershell, elvish)` | Write a shell completion script for the `krab` binary to stdout |
| `krab dev --watch [--release] [--poll-ms <n>] [--settle-ms <n>]` | Watch ["crates/framework/krab_core/src", "crates/framework/krab_macros/src", "services/service_frontend/src", "services/service_frontend_islands/src", "crates/framework/krab_client/src", "services/service_frontend/public"], rebuild changed targets, and restart `service_frontend` |
| `krab dev [--release]` | Build once and run `service_frontend` |
| `krab docs [--out <path>]` | Regenerate this developer workflow document |
| `krab doctor [--diagnostics] [--strict] [--json]` | Run aggregated workspace health checks for project model, env policy, service config, and topology. Checks that do not apply to this project are reported SKIP, not OK. Loads `./.env` first; variables already exported win |
| `krab env-check [--strict] [--json]` | Check the environment policy (auth mode, OIDC settings, environment name). Like `krab doctor`, loads `./.env` first without overriding exported variables |
| `krab gen service <name> --type <rest, graphql, rpc, grpc> [--exposure-mode single, multi] [--path-deps <krab checkout>]` | Scaffold a standalone service crate; `--path-deps` points its `krab_core` dependency at a local checkout instead of crates.io |
| `krab release certify [--out <dir>] [--diagnostics] [--json]` | Run release gates and write a structured evidence bundle (default `<artifact root>/release-certify/local`). Framework checkout only: refuses to run in a generated project |
| `krab security dependency-gate [--diagnostics]` | Run local dependency governance gate with cargo-deny (CI parity) |
| `krab topology doctor [--diagnostics] [--json]` | Run topology boundary checks (cross-service imports, contract payload derives, endpoint config) |
| `krab topology split <domain> [--protocols rest,graphql,rpc,grpc] [--register] [--dry-run]` | Generate split-service extraction skeleton for a domain with adapter stubs and optional registration |
| `krab watch [--release] [--poll-ms <n>] [--settle-ms <n>]` | Alias dedicated to watch workflow |

## Client/WASM Build

- Client package: `service_frontend_islands`
- Crate directory: `services/service_frontend_islands`

`krab build --target client --release` runs:

```sh
wasm-pack build --release --target web --out-dir dist -- --features web
```

`#[island]` compiles its hydrating half only under `feature = "web"`. A bundle built without it still loads and still exports `hydrate` — it just does nothing, at roughly a tenth of the size, with no error anywhere. The CLI passes the feature whenever the client crate's manifest declares it.

## Asset Fingerprinting

When a client/WASM package is configured, the CLI fingerprints browser assets and writes `dist/assets.json`.

## Watch/HMR Workflow

`krab dev --watch` (or `krab watch`) performs incremental change detection over the configured watch roots, rebuilds only the necessary targets, mirrors changed public assets, and writes a lightweight HMR signal file at `dist/.hmr_signal`.

## Bootstrap Health Semantics

`krab bootstrap` starts services in dependency order, waits on each startup readiness probe before proceeding, and applies restart policy backoff/attempt limits from `krab.toml`. Use `/ready` for readiness probes and `/health` for liveness checks. Service stdout/stderr are captured with stable `[service::stream]` prefixes and written to `<artifact root>/orchestrator/` for artifact collection.

## Artifact Root

Commands that write generated artifacts without an explicit `--out` (`krab db rehearsal`, `krab release certify`) and the orchestrator's service logs share one root:

1. `KRAB_ARTIFACT_DIR`, when set to a non-empty value;
2. otherwise `internal/audit/`, when that directory exists in the working directory. This fallback is deprecated, prints a warning, and is removed in 0.7.0;
3. otherwise `.krab/`, which `krab new` adds to the generated `.gitignore`.

## Framework-Only Governance Commands

`krab contract check`, `krab contract protocol-check`, `krab db lifecycle`, `krab db rollback`, `krab db drift`, `krab db rehearsal`, `krab release check` and `krab release certify` validate the Krab framework's own reference services. They run only in a Krab framework checkout, recognised by `crates/framework/krab_core` being a `[workspace] members` entry of `./Cargo.toml` (and existing on disk). Anywhere else they exit non-zero with one message pointing at `cargo test`, `krab doctor --strict`, `krab topology doctor` and `krab security dependency-gate`, which work in any project.
