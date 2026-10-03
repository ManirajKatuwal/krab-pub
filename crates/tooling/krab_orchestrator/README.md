# krab_orchestrator

Multi-process service runner for
[Krab](https://github.com/ManirajKatuwal/krab-pub) workspaces.

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

The orchestrator exits non-zero if `krab.toml` is missing or cannot be parsed,
if the service graph has a cycle or an unknown dependency, or if a service fails
its startup readiness probe.

Per-service stdout and stderr are captured to log files under
`<artifact root>/orchestrator/`, resolved relative to the working directory the
orchestrator is started from. The artifact root is `KRAB_ARTIFACT_DIR` when set;
otherwise `internal/audit/` if that directory exists (deprecated, logged as a
warning, removed in 0.7.0); otherwise `.krab/`, which `krab new` adds to the
generated `.gitignore`. The directory is created automatically.

On Windows every service is placed in a kill-on-close Job Object held by the
orchestrator, so services started through `cargo run` are terminated even if the
orchestrator crashes or is killed before it can shut them down.

## Supervision

Services start in dependency order and stop in reverse. A service that exits on
its own is restarted according to `[services.<name>.restart_policy]`, up to
`max_attempts` within one unstable period; staying up for
`stability_window_ms` (default 60 s) restores the budget, so `max_attempts`
bounds a crash loop rather than a service's lifetime failures. Backoff is
scheduled, not slept through, so one crashed service does not delay supervision
of the others.

See [service composition](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/architecture/service_composition.md#restart-policy)
for the full key reference.

## Configuration

Services are declared in `krab.toml` at the workspace root. See
[service composition](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/architecture/service_composition.md)
for the topology model.

## License

MIT
