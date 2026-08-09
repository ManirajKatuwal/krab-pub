---
name: krab-verify
description: Run the Krab workspace verification gates and record the results as evidence. Use when asked to verify, validate, check gates, prove a change works, prepare a release sign-off, or close a plan — and before claiming any change is done. Covers fmt, clippy, workspace and feature-gated tests, rustdoc, dependency gate, contract/protocol checks, DB lifecycle checks, and topology doctor, with artifact capture into internal/audit/evidence and a ledger row in internal/audit/VERIFICATION_EVIDENCE_LOG.md.
---

# Krab verification gates

Run gates, capture raw output, log the result honestly. Never report a gate as
passing unless you observed its output and exit code.

## 1. Pick the gate set

| Situation | Set |
|---|---|
| Routine change, single crate | **Core** |
| Touched `krab_core` HTTP/auth/protocol, or any service | **Core + Feature** |
| Touched contracts, schemas, or protocol selection | **Core + Feature + Contract** |
| Touched migrations, `db.rs`, or repositories | **Core + Feature + DB** |
| Closing a plan, release sign-off, or audit | **Everything** |

## 2. Set up the bundle

```powershell
$b = "internal/audit/evidence/$(Get-Date -Format yyyy-MM-dd)_<slug>"
New-Item -ItemType Directory -Force $b | Out-Null
git rev-parse --short HEAD | Out-File -Encoding utf8 "$b/_commit.txt"
git status --porcelain | Out-File -Append -Encoding utf8 "$b/_commit.txt"
```

A dirty working tree makes every result in the bundle `partial`. Note it.

## 3. Run

Capture each command with `*>` (stdout **and** stderr) and append the exit code.
The exit code is ground truth — Rust tooling prints `error` lines on success and
succeeds silently.

```powershell
function Gate($slug, $cmd) {
  Invoke-Expression "$cmd *> `"$b/$slug.txt`""
  "exit=$LASTEXITCODE" | Out-File -Append -Encoding utf8 "$b/$slug.txt"
  Write-Output "$slug exit=$LASTEXITCODE"
}
```

### Core

```
fmt_check          cargo fmt --all -- --check
clippy_workspace   cargo clippy --workspace --all-targets -- -D warnings
test_workspace     cargo test --workspace
doc_workspace      cargo doc --workspace --no-deps
dependency_gate    cargo run -p krab_cli -- security dependency-gate --diagnostics
layout_check       python3 scripts/check_workspace_layout.py
```

**Run the gates in the order listed — Core, then Feature, then the rest.**
Cargo keys build artifacts on the resolved feature union, so alternating
between `--workspace` (default features) and `-p krab_core --all-features`
rebuilds the dependency graph *each time you switch*. On 2026-08-08 that turned
a `test_workspace` run into 8741s of which only 267s was actually running
tests; `clippy_workspace` immediately afterwards reused the same cache and took
43s. Group same-feature-set gates together and the whole bundle costs a
fraction of that.

### Prerequisites when running in a container

`rust:latest` ships **without** `rustfmt`, `clippy`, or `cargo-deny`. Omit these
and `fmt_check`, `clippy_workspace`, `dependency_gate`, and every
`release check` / `release certify` step that shells out to them report
**failure for a missing binary, not a real violation** — a false red that is
easy to log as genuine:

```
rustup component add rustfmt clippy
cargo install cargo-deny --locked      # ops-hardening.yaml does the same
```

### Feature

`krab_core` has **no default features** — a bare `cargo test -p krab_core`
compiles a fraction of the surface and proves much less than CI.

```
test_krab_core_all_features       cargo test -p krab_core --all-features
test_krab_core_rest_protocol      cargo test -p krab_core --features rest protocol
test_krab_macros                  cargo test -p krab_macros
test_reference_app                cargo test -p reference_app_islands_rpc
```

The reference app is the only consumer of `#[island]` + `#[server]` together.
Its **browser** half is a separate compilation and is not covered by any native
run — check it explicitly when touching `krab_core`, `krab_macros`, or
`krab_client`:

```
check_reference_app_wasm  cargo clippy -p reference_app_islands_rpc \
                            --target wasm32-unknown-unknown --features web --lib -- -D warnings
```

`#[server]`'s client half went years without compiling because nothing built it.

`krab_macros` uses trybuild. A changed diagnostic means the `.stderr` fixtures in
`crates/framework/krab_macros/tests/compile_fail/` need regenerating —
regenerate deliberately, never blindly.

### Contract

```
contract_check     cargo run -p krab_cli -- contract check --diagnostics
protocol_check     cargo run -p krab_cli -- contract protocol-check --diagnostics
topology_doctor    cargo run -p krab_cli -- topology doctor --diagnostics
```

### DB

```
db_lifecycle   cargo run -p krab_cli -- db lifecycle --diagnostics
db_rollback    cargo run -p krab_cli -- db rollback --diagnostics
db_drift       cargo run -p krab_cli -- db drift --diagnostics
db_rehearsal   cargo run -p krab_cli -- db rehearsal --out internal/audit/evidence/rollback-rehearsal-evidence.txt
```

### Release

```
doctor            cargo run -p krab_cli -- doctor --diagnostics --strict
env_check         cargo run -p krab_cli -- env-check --strict
release_check     cargo run -p krab_cli -- release check --diagnostics --json
release_certify   cargo run -p krab_cli -- release certify --out internal/audit/release-certify/<id> --json
oncall_path       python3 scripts/verify_oncall_delivery_path.py
```

## 4. Summarize markers

```powershell
Select-String -Path "$b/*.txt" -Pattern "^error|FAILED|test result: FAILED|panicked|exit=[1-9]" |
  Out-File -Encoding utf8 "$b/_summary_markers.txt"
```

## 5. Log it

Append one row per command to the ledger in
[`internal/audit/VERIFICATION_EVIDENCE_LOG.md`](../../../internal/audit/VERIFICATION_EVIDENCE_LOG.md):
date, commit SHA, slug, result, artifact path, context, who ran it.

Result vocabulary — use it exactly:

- `pass` — exit 0, artifact captured
- `fail` — non-zero exit, artifact captured
- `partial` — dirty tree, subset of targets, or degraded environment (qualify it)
- `not run` — not executed; **reason mandatory**
- `unverified` — claimed with no artifact; treat as not run

## 6. Report

State per gate: command, result, exit code, artifact path. Then:

- If anything failed: quote the actual error. Do not summarize it away.
- If anything was skipped: say which and why. A skipped gate is never "fine".
- If the tree was dirty: say so, and that results are `partial`.

Do not write "all gates pass" unless every gate in the chosen set exited 0 and
you have the artifact to prove it.

## Rules

1. Run, then log. Never the reverse.
2. Match CI's feature flags, or the local result does not predict CI.
3. Evidence from an earlier commit does not attest the current one.
4. Never fix a gate by weakening it — no `#[allow]` sprinkling, no `deny.toml`
   `ignore` entries (that array stays empty), no `--no-verify`.
5. `internal/` is gitignored. Bundles are local artifacts; reference them by path.
