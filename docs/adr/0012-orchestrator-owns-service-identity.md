# ADR 0012: The Orchestrator Owns Service Identity

## Status

**Accepted** — 2026-09-07, by the repository owner. Implemented in the same
change, released in `0.5.0`.

> Corrected 2026-09-09: this line said "0.4.x". The decision and its reasoning
> are unchanged — only the release number was wrong, having been written while
> the change sat unreleased on `main`. `0.4.0` shipped 2026-08-12, before this
> ADR existed.

## Context

`KRAB_PORT` and `KRAB_SERVICE_NAME` read as per-service settings and behave as
workspace-wide ones.

`krab_core::config::parse_port_from_env` returns the `default_port` a binary
passes to `KrabConfig::from_env_checked` **only when `KRAB_PORT` is unset**. The
per-service default is a fallback, not a floor. So `service_auth` (3001),
`service_users` (3002), `service_frontend` (3000) and `service_users_split`
(3207) all collapse onto one ambient value the moment it exists. Verified:
`KRAB_PORT=13207` makes `service_frontend` log `"port":13207`.
`KRAB_SERVICE_NAME` is the same defect, applied to the `service` field on every
log line and metric (`telemetry`), the `KRAB_PROTOCOL_ENABLED_<NAME>` lookup
(`protocol`), and migration attribution (`db::postgres`).

The orchestrator inherits its own environment and merges `[services.X].env`
over it — there is no `env_clear()` anywhere in the workspace — so an ambient
value reaches every child. Meanwhile `krab.toml` pinned `KRAB_PORT` for zero of
four services while hardcoding 3001/3002/3000/3207 into the health-probe URLs.
The intended topology was asserted in the probes and never told to the
children.

The failure that produces is not a port conflict. With `KRAB_PORT=3000`
exported, `krab bootstrap` reports a **readiness-probe timeout on auth at
:3001** — no bind error, no mention of ports, and a child serving happily on
3000. `krab_core::service` logs `service_listening` *before* attempting the
bind, so a service that never bound anything still says it is listening. Only
when the ambient value collides with an already-taken port does the honest
`os error 10048` appear. `KRAB_SERVICE_NAME` does not even fail: every service
simply reports `krab`, which is what `.env.example` shipped uncommented.

Nothing validated any of this: not `env_policy`, not `krab topology doctor`, no
CI gate.

A second defect was found while fixing the first. The orchestrator parsed
`krab.toml` with the `config` crate, which lowercases every key it reads from a
file. `[services.X].env` entries were therefore delivered to children with
mangled names — `RUST_LOG = "info"` arrived as `rust_log`. Windows environment
variables are case-insensitive, so this was invisible on the maintainer's
platform and total on Linux and macOS, including in containers and CI. It also
means the obvious fix below — pinning the variables as strings in `krab.toml` —
was not merely weaker than the chosen one, it did not work at all off Windows.

## Decision

1. **`ServiceDefinition` gains two typed fields**, `port: Option<u16>` and
   `service_name: Option<String>`. `service_name` defaults to the
   `[services.<key>]` table key, which is already the name the operator gave
   that service; `port` has no default, because there is no honest one to
   invent.
2. **The orchestrator injects them at spawn.** Precedence, lowest first:
   the inherited environment, then the injected identity, then explicit
   `[services.X].env` entries. Explicit entries win last so a manifest can
   still say what the typed fields cannot, and so existing pins keep working.
   The injected layer sits above inheritance, which is what stops an ambient
   `KRAB_PORT` moving a service out from under its own health probe.
3. **The manifest is validated before anything is spawned.** Two services
   declaring one port is rejected by name; two services resolving to one
   `service_name` is rejected by name; `port = 0` is rejected. A declared port
   that disagrees with the port in that service's probe URL is warned about,
   not rejected — a probe may legitimately address a proxy. `krab topology
   doctor` performs the duplicate-port check statically, so it is a gate and
   not only a startup check.
4. **A service that declares no port still inherits one**, unchanged, and logs
   `service_port_unpinned_inheriting_ambient_krab_port` when it does. Nothing
   is invented for manifests written before this field existed.
5. **`krab.toml` declares all four services' ports and names**, and the
   orchestrator parses it with `toml` rather than `config`, so the keys it
   reads are the keys the manifest wrote.

## Consequences

- An exported `KRAB_PORT` or `KRAB_SERVICE_NAME` no longer changes where an
  orchestrated service listens or what it calls itself. A service run directly
  (`cargo run --bin service_auth`) still takes the ambient value — that is the
  documented behaviour of the variable, and the orchestrator is the thing with
  standing to override it.
- **Service identity now defaults to the manifest key.** A downstream
  `krab.toml` whose `[services.api]` runs a binary defaulting to `backend` will
  see its telemetry `service` field change from `backend` to `api` on upgrade.
  The fix is one line — `service_name = "backend"` — and the alternative was
  leaving `KRAB_SERVICE_NAME` unpinned for every service that had not opted in,
  which is the defect.
- **`[services.X].env` keys keep their case**, so entries that were silently
  inert on Linux and macOS now take effect. A manifest that has been carrying a
  broken-but-harmless `env` entry will find it applied for the first time.
- `krab_orchestrator` drops its `config` dependency for `toml`, the parser
  `krab doctor` and `krab topology doctor` already use on this file. Non-TOML
  manifests (`krab.yaml`, `krab.json`) are no longer accepted — a format
  `config`'s `File::with_name` allowed incidentally, which nothing in the
  workspace, the documentation, or the templates has ever produced.
- The health probes and the processes behind them can no longer disagree
  silently. They can still be *made* to disagree by an explicit `env` entry,
  which is deliberate, and which the probe-port warning surfaces.

## Alternatives considered

**Pin `KRAB_PORT` and `KRAB_SERVICE_NAME` as strings in `[services.X].env`.**
The smallest change, and the one already applied to `service_users_split`. It
was rejected on three counts. It cannot be validated — a duplicate port is just
two matching strings in a map the orchestrator has no opinion about, so the
operator error that produces the confusing readiness timeout goes uncaught. It
puts the topology in two places, `env` and the probe URL, with nothing checking
that they agree. And, as it turned out, `config` lowercased those keys, so on
Linux and macOS the pin did nothing whatsoever; the defect it was meant to fix
would have stayed open behind a fix that looked applied.

**Service-scoped variables (`KRAB_AUTH_PORT`, `KRAB_USERS_PORT`), following the
existing `KRAB_AUTH_BASE_URL` precedent.** This is a real precedent and it
solves the collision, but it solves it in the wrong crate: every service would
have to know its own scope prefix and consult a second variable, which makes
`krab_core`'s config surface grow one variable per service per knob and pushes
topology into the framework's environment vocabulary. It also does nothing for
a service run outside the orchestrator, which is the case that variable would
exist for. The orchestrator already knows the topology — it asserts it in every
probe URL — so it is the component with both the knowledge and the authority.

**Remove `KRAB_PORT` from `.env.example` and leave the rest.** This treats a
documentation line as the cause. The variable would still override every
service at once for anyone who set it from a shell, a Dockerfile, a CI matrix,
or a Kubernetes manifest; the reference services would still be unpinned; and
the confusing readiness-timeout failure mode would be unchanged. `.env.example`
is fixed here too — both lines are commented out with an explanation — but as a
consequence of the decision, not as the decision.

## References

- Manifest schema and validation:
  `crates/tooling/krab_orchestrator/src/configuration.rs`
- Injection at spawn: `crates/tooling/krab_orchestrator/src/process_runtime.rs`
  (`build_command`)
- Static duplicate-port gate: `crates/tooling/krab_cli/src/topology.rs`
  (`detect_service_config_violations`)
- The variables themselves: `crates/framework/krab_core/src/config.rs`
  (`parse_port_from_env`, `KrabConfig::from_env_checked`)
- Operator-facing symptom:
  [troubleshooting guide](../guides/troubleshooting.md) → Port conflicts
- Variable reference: [environment.md](../reference/environment.md) → Core service
