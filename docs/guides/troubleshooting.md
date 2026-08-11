# Troubleshooting

Diagnose the failures developers actually hit while building with Krab: islands
that render but never become interactive, services that refuse to start, and CI
gates that disagree with a local run.

This guide is for development. For production incidents — alerts firing, SLO
burn, rollback decisions — use the
[on-call playbook](../operations/oncall_playbook.md) instead.

---

## Triage matrix

| Symptom | Likely layer | Start here |
|---|---|---|
| Island renders but is not interactive | WASM bundle / hydration | [`crates/framework/krab_client/src/lib.rs`](../../crates/framework/krab_client/src/lib.rs), browser console |
| Route returns unexpected 404/500 | Service router | [`services/service_frontend/src/main.rs`](../../services/service_frontend/src/main.rs) and its generated route registration ([`build.rs`](../../services/service_frontend/build.rs)) |
| Service exits at startup | Config validation | [`crates/framework/krab_core/src/config.rs`](../../crates/framework/krab_core/src/config.rs), [`.env.example`](../../.env.example) |
| `/ready` fails while `/health` passes | Downstream dependency | [`krab.toml`](../../krab.toml) dependency wiring, dependency service logs |
| Contract or protocol gate fails | CLI governance | `cargo run -p krab_cli -- contract check --diagnostics` |
| DB lifecycle gate fails | Migration governance | [`crates/framework/krab_core/src/db/`](../../crates/framework/krab_core/src/db/), [database reference](../reference/database.md) |
| Compile error: module not found in `krab_core` | Feature gating | [Feature-set mismatch](#a-ci-gate-fails-or-passes-locally-but-not-in-ci) |

---

## Islands render but are not interactive

Server-side rendering succeeded — the HTML is there — but clicking does
nothing. Hydration is a separate pipeline with its own failure points. Work
through them in order.

### 1. Is the WASM bundle built and served?

The browser half of an island only exists if the bundle was built **for wasm32
with the `web` feature**:

```sh
wasm-pack build --target web -- --features web
```

Then the page must load it (`<script type="module" src="/pkg/<crate>.js"></script>`)
and the server must serve the `pkg/` directory as static files. Open the
browser's network tab: a 404 on the `.js` or `_bg.wasm` request means the bundle
is missing or the static route is not wired. `krab_core::static_assets` provides
`resolve_static_pkg_path` for serving `pkg/` safely.

`--features web` is required and only valid on wasm32 — `#[island]` selects its
browser half on that feature, and enabling `web` for a native build does not
compile. See [Getting Started §4](getting_started.md#4-your-first-island).

### 2. What does the browser console say?

The hydration runtime in
[`krab_client`](../../crates/framework/krab_client/src/lib.rs) logs structured
diagnostics to the console rather than failing silently:

- `Hydrating Krab app...` — the bundle loaded and hydration started. If this
  line is absent, the problem is step 1, not hydration.
- `Hydrating island: <name>` — per-island progress.
- JSON-shaped warnings/errors with a `scope` field — decode failures, missing
  registrations, and hydration mismatches, each naming the island and boundary.

### 3. Check the marker attributes in the DOM

Every island wrapper carries the markers the runtime queries on
([ADR 0001](../adr/0001-hydration-markers.md)):

| Attribute | Meaning |
|---|---|
| `data-island` | Island name; must match an `#[island]` registered in the bundle |
| `data-props` | Serialized props; must decode into the island's props struct |
| `data-krab-node-id` | Per-node hydration path within the boundary |
| `data-krab-boundary-state` | Live hydration status (below) |

`data-krab-boundary-state` is the fastest signal. The runtime sets it to
`hydrating`, then resolves it:

| State | Meaning |
|---|---|
| `ok` | Hydrated cleanly |
| `patched` | Hydrated, but server and client markup disagreed and nodes were patched — check the console for mismatch warnings |
| `decode-error` | `data-props` did not deserialize into the props struct; a deterministic fallback node was rendered |
| `missing-definition` | `data-island` names an island the bundle never registered — usually a stale bundle or a renamed island |
| `error` | The island's render panicked or threw; see the console error |

A state stuck at `ssr` means the runtime never reached that island; a state
stuck at `hydrating` means it started and died — the console has the reason.

### 4. Common causes, in observed order

1. Stale bundle: the island was renamed or its props struct changed, but
   `wasm-pack build` was not re-run.
2. Props struct not `Clone + Serialize + Deserialize`, or server and client
   compiled from different revisions, so `data-props` no longer decodes.
3. Bundle built without `--features web`, so no hydrators were registered.
4. Server markup and client render disagree (`patched` state) — typically
   non-deterministic render output such as timestamps computed at render time.

For how the pipeline is supposed to work end to end, see
[hydration.md](../architecture/hydration.md). A known-good working example is
the vendored reference app:
[`examples/reference_apps/islands_rpc/`](../../examples/reference_apps/islands_rpc/).

---

## A service won't start

Krab services fail fast and loudly: startup returns `anyhow::Result`, so the
process exits with a message rather than limping. Read the message first — it
usually names the exact variable.

### Configuration validation

Configuration loads via `KrabConfig::from_env_checked` and is checked by
`validate()` in
[`krab_core/src/config.rs`](../../crates/framework/krab_core/src/config.rs).
In `dev`, checks are skipped. In `staging`, `prod`, or any unrecognised
`KRAB_ENVIRONMENT`, startup rejects:

- empty `KRAB_CORS_ORIGINS` (no wildcard CORS outside dev),
- `KRAB_AUTH_MODE=static` or a non-empty `KRAB_BEARER_TOKEN`,
- inline secrets — `KRAB_JWT_SECRET` and friends must arrive via `*_FILE` or
  `*_VAULT_REF` sourcing,
- missing JWT/OIDC provider configuration.

A service that runs locally and dies in staging almost always tripped one of
these. Two CLI commands reproduce the checks without booting a service:

```sh
cargo run -p krab_cli -- env-check --strict
cargo run -p krab_cli -- doctor --diagnostics --strict
```

Compare your `.env` against [`.env.example`](../../.env.example) — every
supported variable is documented there and in
[environment.md](../reference/environment.md).

### Port conflicts

An unparseable `KRAB_PORT` is rejected at startup with the offending value. A
*taken* port fails at bind time instead. Defaults:

| Port | Service |
|---|---|
| 3000 | `service_frontend` |
| 3001 | `service_auth` |
| 3002 | `service_users` |
| 3100 | `islands_rpc` reference app |
| 3207 | `service_users_split` |

On Windows, find the holder with
`Get-NetTCPConnection -LocalPort 3000 | Select-Object OwningProcess`; on
Linux/macOS, `lsof -i :3000`. A common cause is a previous orchestrator run
that left a service behind.

### Database connectivity

Driver selection is `KRAB_DB_DRIVER=postgres` (default) or `sqlite`, resolved
by [`krab_core::db`](../../crates/framework/krab_core/src/db/). Check, in
order: `DATABASE_URL` is set and points at a reachable server; the database
exists; and the crate was compiled with the matching feature (`db-postgres` /
`db-sqlite`) — compiling a driver in and selecting one at runtime are separate
decisions. See [database.md](../reference/database.md).

### Feature flags

`krab_core` has **no default features**. If a build fails with "unresolved
module" or a missing type, the module is probably feature-gated: `http*`
modules need `rest`, `credentials` needs `auth`, `db` needs a driver feature.
The full table is in
[Getting Started § Feature flags](getting_started.md#feature-flags).

---

## A CI gate fails (or passes locally but not in CI)

### Feature-set mismatch

The most common divergence. `cargo test -p krab_core` with **no features**
compiles a much smaller surface than the gates run — entire modules
(`http`, `server_fn`, `db`, `credentials`, …) are compiled out, so their tests
silently do not run. Match the feature set the gate uses before trusting a
local pass:

```sh
cargo test --workspace
cargo test -p krab_core --features rest
cargo test -p krab_core --all-features
cargo test -p krab_macros            # trybuild compile-fail suite
```

### Strictness mismatch

CI runs formatting and lints in their strict forms. Run the same commands, not
softer ones:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

A warning that is tolerable locally is a hard failure under `-D warnings`, and
`--all-targets` lints tests and benches that a bare `cargo clippy` skips.

### Reproduce the gate itself

Every governance workflow in
[`.github/workflows/`](../../.github/workflows/) runs the same binaries you
can run locally:

```sh
cargo run -p krab_cli -- contract check --diagnostics
cargo run -p krab_cli -- contract protocol-check --diagnostics
cargo run -p krab_cli -- db lifecycle --diagnostics
cargo run -p krab_cli -- db drift --diagnostics
cargo run -p krab_cli -- topology doctor --diagnostics
cargo run -p krab_cli -- security dependency-gate --diagnostics
```

Open the failing workflow file, find the command it runs, and run that exact
command. The generated [dev workflow guide](dev_workflow.md) lists the full
local verification sequence.

---

## Systematic recovery checklist

When nothing above matches, work top-down instead of guessing:

1. **Isolate the layer** with the triage matrix — server render, hydration,
   config, database, or CI policy.
2. **Reproduce with the smallest command** — a single `cargo test -p <crate>
   --features <set>` or a single `curl`, not the whole orchestrator.
3. **Read the structured logs** — services log with `tracing` and stable
   snake_case event names; the browser runtime logs JSON diagnostics.
4. **Diff against a known-good baseline** — `.env.example` for config, the
   `islands_rpc` reference app for island wiring, the workflow file for CI.
5. **Fix, then re-run the gate that caught it** — with the exact feature set
   and flags CI uses, before considering it closed.

Still stuck on something that looks like an operational failure rather than a
development one? That is the [on-call playbook](../operations/oncall_playbook.md)'s
territory.
