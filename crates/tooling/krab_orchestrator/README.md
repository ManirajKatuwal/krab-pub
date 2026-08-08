# krab_orchestrator

Multi-process service runner for
[Krab](https://github.com/krab-framework/krab) workspaces.

The orchestrator reads `krab.toml`, starts every declared service as a
supervised child process, and manages them as one unit: health-aware startup
ordering, deterministic restart policy, crash-cause reporting, and file-watch
driven rebuilds during development.

## Usage

```sh
cargo run --bin krab_orchestrator
```

Or via the CLI, which builds first:

```sh
krab bootstrap
```

Per-service stdout and stderr are written under `internal/audit/orchestrator/`.

## Configuration

Services are declared in `krab.toml` at the workspace root. See
[service composition](https://github.com/krab-framework/krab/blob/main/docs/architecture/service_composition.md)
for the topology model.

## License

MIT
