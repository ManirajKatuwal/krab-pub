# Release checklist — 0.5.0

The security-and-correctness release. Runbook: work the phases in order, and
treat a phase's "done when" as the only thing that closes it.

## Why 0.5.0

Four of the changes are breaking, and on `0.x` the **minor** field is the
compatibility boundary — `^0.4` and `^0.5` do not unify. See
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md#while-the-version-is-below-10).

| Change | Who it affects |
|---|---|
| `/metrics` and `/metrics/prometheus` are no longer default open paths | Every deployment scraping them anonymously — set `KRAB_METRICS_PUBLIC=true` |
| A service's identity defaults to its `krab.toml` key | Anyone whose `[services.X]` key differs from the binary's own default name — telemetry `service=` changes |
| The orchestrator reads `krab.toml` only, not `krab.yaml`/`krab.json` | Anyone who relied on `config`'s incidental format sniffing |
| `krab_core::render_stream`'s streaming writer is not compiled for `wasm32` | Nobody with working code — any browser call into the writer already panicked. The marker parser (`SuspenseMarker`, `is_finalized_ssr_snapshot`) stays available on `wasm32`; an earlier cut gated the whole module and the review caught it |
| `krab_core::http::ErrorCategory` is `#[non_exhaustive]` | Rust callers that `match` on a category without a wildcard arm |

Deprecated in this release and removed in `0.6.0`:
`krab_core::db::postgres::run_migrations` and
`krab_core::render_stream::SuspenseMarker`. Both carry
`#[deprecated(since = "0.5.0")]`
(`crates/framework/krab_core/src/db/postgres.rs:609`,
`crates/framework/krab_core/src/render_stream.rs:60`), which is why
`next_version` must be `0.6.0` — see Phase 1.

## What gates this release, and what does not

**No GitHub Actions run has ever succeeded on either repository.** As of
2026-09-09: the private development repository has 175 runs and **0**
successes, the public one 111 and **0**. The cause is account-level (billing);
runs fail at startup with zero jobs, unattributed to any workflow. `v0.5.0` was
the first `v*` tag ever pushed to the development remote (`v0.2.0`–`v0.4.0`
exist only on the public mirror); it produced exactly such a run, so
[`release-attestation.yaml`](../../.github/workflows/release-attestation.yaml)
(`push: tags: v*`) is proven reachable and proven blocked, and has still never
executed.

So for this release:

| Claim | True? |
|---|---|
| "Enforced on push by CI" | **No.** Not for any gate, in either repo |
| "Verified in a containerised run and recorded in the evidence ledger" | Yes — this is what Phases 3-5 produce |
| "Attested by `release-attestation.yaml`" | **No.** The workflow has never triggered |

Do not write CI run links into the release notes. Write evidence-bundle paths.
The convention is `internal/audit/evidence/<date>_<name>/`, one file per gate
with `exit=` appended, indexed by a row in
`internal/audit/VERIFICATION_EVIDENCE_LOG.md`. (`internal/` is gitignored and
absent from a fresh clone; if you do not have it, run the commands directly and
keep the output wherever your checkout can hold it.)

---

## Phase 1 — Freeze the version surface

### 1.1 Bump the workspace version

Four places in the root [`Cargo.toml`](../../Cargo.toml), plus the lockfile:

| What | Where |
|---|---|
| `[workspace.package] version` | line 31 |
| `[workspace.dependencies] krab_core` | line 57 |
| `[workspace.dependencies] krab_macros` | line 58 |
| `[workspace.dependencies] krab_client` | line 69 |
| `Cargo.lock` | `cargo check --workspace` rewrites it |

Cargo has no `version.workspace = true` inside `[workspace.dependencies]`, so
those three pins are duplicated by necessity and drift silently — a stale pin
surfaces only at `cargo publish`, which is the worst moment to find out.

### 1.2 The `next_version` trap

**Move `[workspace.metadata.krab] next_version` to `0.6.0` in the same commit as
the version bump.** It is at [`Cargo.toml`](../../Cargo.toml) lines 27-28,
directly above `[workspace.package] version` at lines 30-31.

The mechanism, in [`scripts/check_workspace_layout.py`](../../scripts/check_workspace_layout.py):

- Lines 164-168 read `[workspace.package] version` as `current` and
  `[workspace.metadata.krab] next_version` as `next_version`.
- Lines 175-180 fail outright if the two are equal:

  > `[workspace.metadata.krab] next_version is '0.5.0', the version already in
  > [workspace.package]. After a release, move it forward to the next planned
  > version`

- Lines 189-217 then use `next_version` as a **ceiling**: any
  `#[deprecated(since = ..)]` under `crates/framework`, `crates/tooling`,
  `services` (lines 192-205) or any `As of X` in `docs/reference/api.md`
  (lines 207-216) naming a version beyond it is a failure. This release
  deprecates two items with `since = "0.5.0"`, and
  [`docs/reference/api.md`](../reference/api.md) carries three `As of 0.5.0`
  notes — with `next_version` left at `0.5.0` those still pass the ceiling, but
  the equality check at line 175 fails first, so bumping `version` without
  bumping `next_version` fails the script every time.
- `main()` runs the check unconditionally at line 259; there is no opt-out.

This is not a soft check. It is the **first step of the first job** of
[`ops-hardening.yaml`](../../.github/workflows/ops-hardening.yaml) —
`Workspace layout check`, lines 43-44 of `lint-format-security`, before `fmt`,
before `clippy`, before anything compiles. (That job has never run on push; the
containerised gate set in Phase 3 runs the same script first for the same
reason.)

### 1.3 Verify

```sh
python3 scripts/check_workspace_layout.py
```

**Done when** it prints `OK: workspace layout checks passed (10 members)` and
exits 0.

---

## Phase 2 — Changelog, docs, and migration guidance

### 2.1 Changelog

Move the `[Unreleased]` body under a `## [0.5.0] — <date>` heading, leave
`[Unreleased]` present and empty, and keep the Added / Changed / Deprecated /
Fixed / Security / Governance sections that
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) requires.

**Done when** `grep -n "^## \[" CHANGELOG.md` shows `[Unreleased]` immediately
above `[0.5.0]`.

### 2.1b Record the breaking-change policy deviation

`RELEASE_POLICY.md` §"Breaking Change Policy" asks for three things. This release
delivers one of them, and the other two were never possible:

| Requirement | 0.5.0 |
|---|---|
| Migration guidance in `docs/reference/api.md` | **Met** — notes for all three |
| Deprecation notice in the **previous** release's changelog | **Not met.** `0.4.0` announced none of them |
| One minor version of deprecation warning before removal | **Not met.** All three land in one release |

That is a deviation, and it is recorded rather than quietly taken:

- **Metrics closing** is a security fix. Announcing it a release early means
  leaving every deployment's route inventory, traffic volumes and latency
  distributions anonymously readable for another minor — the deprecation period
  would itself be the exposure.
- **Orchestrator identity** fixed a live defect: every service reported the same
  `service` label, and one ambient `KRAB_PORT` moved them all onto one port. A
  deprecation window means shipping a release that keeps a known-wrong telemetry
  label and a readiness-probe failure mode.
- **The `render_stream` writer on `wasm32`** had nothing to deprecate. Any call
  was a guaranteed runtime panic. The marker parser, which did work on `wasm32`,
  is not part of the break — the gate was moved inside the module after the
  review found the first cut would have removed it.

**Done when** the `[0.5.0]` changelog preamble states the deviation, so a reader
who checks the policy against the release finds the reasoning rather than an
apparent violation.

### 2.2 Migration guidance for every breaking change

`RELEASE_POLICY.md` §"Breaking Change Policy" requires migration guidance, and
the `[0.5.0]` changelog preamble links to
[`docs/guides/migration_guide.md`](../guides/migration_guide.md).

> **Closed in the release commit (`5f4a265`).** When this checklist was drafted
> the guide had no `0.5.0` section and two sections mis-headed `Unreleased` that
> described shipped 0.2.0 work. Both were fixed in the same commit: the guide now
> opens with `### 0.4.0 → 0.5.0`, and the two stale headings read `0.2.0`.

Verify the section covers every breaking change from the table at the top of this
document. The metrics one is the only one an operator must act on before
deploying, so it leads.

**Done when** `grep -n "0\.5\.0" docs/guides/migration_guide.md` returns a
heading for each of the five rows, or an explicit note for the ones needing no
action.

(Those two headings predated this release.
Note them; do not renumber them here.)

### 2.3 Version references users will actually read

The published crate front pages carry install snippets, and they were stale —
every one of them, by up to four releases. **Fixed in the release commit
(`5f4a265`); all five now read `0.5`.** Re-check them at the next release,
because nothing enforces this:

| File | Says | Should say |
|---|---|---|
| `crates/framework/krab_client/README.md:41` | `krab_client = "0.4"` | `"0.5"` |
| `crates/framework/krab_client/README.md:64` | `version = "0.4"` | `"0.5"` |
| `crates/framework/krab_core/README.md:42` | `version = "0.1"` | `"0.5"` |
| `crates/framework/krab_macros/README.md:20` | `krab_macros = "0.1"` | `"0.5"` |
| `docs/guides/getting_started.md:166-167` | `version = "0.2"` | `"0.5"` |

The crate READMEs are the `readme` field of published packages, so a wrong pin
there is shipped to docs.rs and crates.io, not just to this repository.

Also check `CLAUDE.md` (`- Version:`), `README.md`, and
`docs/reference/database.md` for version pins and deprecation windows — those
three needed edits at 0.4.0 for the same reason.

**Done when** no tracked file outside `CHANGELOG.md`, the release checklists, or
a deliberate historical note claims `0.4.0` is current.

### 2.4 Index this document

Add a row for this checklist to [`docs/README.md`](../README.md) and demote the
`0.4.0` row from "the current release" to a reference entry.

---

## Phase 3 — Run the gate surface

Run on a settled tree with no other `cargo` process holding the build lock. The
container is `krab-ci`, which bind-mounts the repo at `/work` with
`CARGO_TARGET_DIR=/target` on a separate volume:

```sh
docker start krab-ci
MSYS_NO_PATHCONV=1 docker exec krab-ci bash -lc "cd /work && ..."
```

`MSYS_NO_PATHCONV=1` is required from Git Bash, or `/work` is rewritten into a
Windows path and nothing is found.

The gate set:

```sh
python3 scripts/check_workspace_layout.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p krab_core --all-features
cargo test -p krab_core --features db-postgres
cargo test -p krab_core --features db-sqlite
cargo test -p krab_macros
cargo test -p krab_cli
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
python3 scripts/verify_oncall_delivery_path.py
cargo run --package krab_cli -- security dependency-gate --diagnostics
```

`cargo test --workspace` alone proves very little: `krab_core` has no default
features, so it compiles a fraction of the surface. The `--all-features` run is
the one that matters, and the two single-driver runs are compile-isolation
checks — neither is database coverage.

Three commands need environment that a bare container does not have:

```sh
# needs the Postgres sidecar
docker start krab-ci-postgres
export KRAB_TEST_DATABASE_URL=postgres://postgres:ci_db_test_secret@172.17.0.3:5432/krab_test
export KRAB_REQUIRE_DB_TESTS=1
cargo run -p krab_cli -- db rehearsal

# need KRAB_AUTH_MODE=jwt with issuer and audience set
cargo run -p krab_cli -- doctor --diagnostics --strict
cargo run -p krab_cli -- env-check --strict
```

> Note: no workflow invokes `doctor`, `env-check`, or `topology doctor`. They
> are release gates by policy, not by CI, and always have been.

And the two that need a browser:

```sh
cargo install wasm-bindgen-cli --version 0.2.127 --locked   # must match Cargo.lock
CHROMEDRIVER=/path/to/chromedriver \
  cargo test -p krab_client --target wasm32-unknown-unknown --features web

wasm-pack build examples/reference_apps/islands_rpc --target web -- --features web
```

`wasm-bindgen-cli` must match the `wasm-bindgen` version in `Cargo.lock`
(`0.2.127`, verified) or the module will not load.

**Done when** every command above has exit 0 captured in a file under the
evidence bundle, and a ledger row records the commit SHA and the tree state at
the time of the run.

---

## Phase 4 — The publish dry-run, on an LF checkout

**First use this release.** This gate has not been attested since before 0.4.0.

`cargo publish` refuses to package a dirty tree. This worktree is CRLF against
an LF index — `git ls-files --eol` reports **184** files as `i/lf w/crlf` plus 4
`w/mixed`, verified 2026-09-09 — so Linux git inside the container reads the
whole tree as modified. At 0.4.0 the gate failed with

```
error: 25 files in the working directory contain changes that were not yet
committed into git
```

(25 because `cargo publish` only lists the files inside the package it is
packaging; the repo-wide figure is the 184.) The discriminator is:

```sh
git diff --ignore-cr-at-eol --stat
```

If that is empty — or shows only files you actually edited — while
`git status --porcelain` shows ~190, the dirty read is line endings and nothing
else. **Both obvious routes are closed:**

- **Container against the bind-mounted worktree** — fails on the CRLF read
  above.
- **The Windows host** — no MSVC toolchain. `cl.exe` is absent, the only
  `link.exe` on `PATH` is Git's coreutils `link`, and the machine-local
  `~/.cargo/config.toml` points the linker at `rust-lld`, which links pure Rust
  but not the C in `ring` / `libsqlite3-sys`. The compile `--dry-run` performs
  cannot complete.

**The remedy is an LF checkout**: clone inside the container, so Linux git
writes the working tree itself and there is no CRLF to misread. This needs no
network and no host toolchain, and it packages the *committed* state, which is
what a publish attests anyway.

```sh
docker start krab-ci

MSYS_NO_PATHCONV=1 docker exec krab-ci bash -lc '
  set -e
  SHA=$(git -C /work rev-parse HEAD)
  rm -rf /src/krab && mkdir -p /src
  git clone /work /src/krab
  cd /src/krab && git checkout "$SHA"
  git status --porcelain            # MUST be empty
  export CARGO_TARGET_DIR=/target-lf
  cargo publish --workspace --dry-run
'
```

`--workspace` rather than five per-crate runs: the sibling crates are not on
crates.io at this version yet, so a per-crate `--dry-run` fails with
`no matching package named krab_core`. The workspace form resolves siblings
against the locally packaged versions and verifies them together, in dependency
order.

**Done when** `git status --porcelain` in the clone is empty *and*
`cargo publish --workspace --dry-run` exits 0, with both outputs in the evidence
bundle. If the clone is not clean, stop — that is a real uncommitted change,
not the CRLF artifact.

---

## Phase 5 — Governance evidence

### 5.1 `krab release certify` — attempt 4

```sh
cargo run -p krab_cli -- release certify \
  --out internal/audit/release-certify/release-0.5.0 --json
```

It runs, in order: `release-check`, `fmt-check`, `clippy`, `workspace-tests`,
`contract-checks`, `protocol-contract-checks`, `db-lifecycle`, `db-drift`,
`db-rollback-rehearsal`, and writes `summary.json` / `summary.md` under
`08-signoff/`, indexed at `internal/audit/release-certify/latest.json`.

**No bundle has ever been produced. Three attempts, three distinct causes**
(recorded in `internal/audit/release-certify/README.md` and its `RESULTS.txt`):

1. Bare `rust:latest`: `dependency_gate`, `clippy`, and `fmt-check` all reported
   `failed` — but for *missing binaries* (`no such command: deny`, components
   not installed), not policy violations. A bundle asserting three false
   failures is worse than none, so it was deleted.
2. Prerequisites installed; Docker Desktop's engine died mid-`cargo-deny`
   compile (every API call `500 Internal Server Error`) and took the container
   with it.
3. **2026-08-11**, the most recent: prerequisites were fine (`cargo-deny 0.20.2`,
   `rustfmt 1.9.0`, `clippy 0.1.97`, all recorded in `prereqs.txt`) but both
   `release check` and `release certify` exited **101** with
   `error: could not find Cargo.toml in /work or any parent directory` — the
   container ran without the repo bind-mount. Zero-byte JSON, no bundle.

So before attempt 4, confirm all five:

- [ ] `docker version` returns a server version (attempt 2).
- [ ] `rustfmt`, `clippy`, and `cargo-deny` are on `PATH` inside the container —
      reuse the `krab-cargo-bin` volume rather than recompiling `cargo-deny`
      (attempt 1).
- [ ] `/work/Cargo.toml` exists inside the container *before* invoking the CLI
      (attempt 3). Check it; do not assume the mount.
- [ ] A reachable Postgres. `db-rollback-rehearsal` sets `KRAB_REQUIRE_DB_TESTS=1`
      itself (`crates/tooling/krab_cli/src/release_ops.rs:158-183`), so an
      unreachable database now *fails* instead of silently passing — this is new
      since 0.4.0 and would sink attempt 4 on its own.
- [ ] A clean commit, not a dirty tree — the bundle attests a tree state.

**Done when** `summary.json` has `"success": true`, or, if it does not, the
failing step is recorded honestly in the ledger with its artifact. A certify run
that produced no bundle is `not run`, never `pass`.

### 5.2 Rollback rehearsal

```sh
KRAB_REQUIRE_DB_TESTS=1 \
KRAB_TEST_DATABASE_URL=postgres://... \
  cargo run -p krab_cli -- db rehearsal
```

Underneath it is
`cargo test -p krab_core --features "db-postgres rest" test_migration_rollback -- --nocapture`.
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) §"Operational Readiness" requires
rehearsal evidence for the current migration version.

**Done when** `internal/audit/evidence/rollback-rehearsal-evidence.txt` exists
and was written by *this* run. If the rehearsal did not run, no file is written
— that is the intended behaviour, and an absent file is the honest result.

### 5.3 Evidence bundle

```sh
python3 scripts/release_evidence_bundle.py
```

Writes `benchmarks/release_evidence_bundle.{json,md}` with a present/absent
table over the NFT summaries, replica results, shared-state validation, trend
history, and the rollback rehearsal evidence. It reports `WARN` rather than
failing when items are missing.

**Done when** the bundle is generated and every `missing` entry is either
resolved or explained in the release notes. A `WARN` you have read and accepted
is fine; a `WARN` nobody looked at is not.

### 5.4 Ledger row

Record every command from Phases 3-5 in
`internal/audit/VERIFICATION_EVIDENCE_LOG.md`: one row per command per commit,
with the commit SHA at the time of the run, the real result
(`pass` / `fail` / `partial` / `not run`), and an artifact path. Feature flags
are part of the command. A `pass` with no artifact is downgraded to
`unverified`.

**Done when** every claim in the release notes maps to a row.

---

## Phase 6 — Publish

Five crates are published. The other five workspace members carry
`publish = false` and are not released — `services/service_auth`,
`services/service_frontend`, `services/service_users`,
`services/service_users_split`, and `examples/reference_apps/islands_rpc`
(verified in their manifests).

Order comes from the dependency graph in the manifests:

| Order | Crate | Depends on |
|---|---|---|
| 1 | `krab_macros` | — |
| 2 | `krab_core` | — |
| 3 | `krab_client` | `krab_core` (features `web`), `krab_macros` |
| 4 | `krab_cli` | `krab_core` (features `auth`) |
| 5 | `krab_orchestrator` | `krab_core` |

```sh
cargo publish -p krab_macros
cargo publish -p krab_core
cargo publish -p krab_client
cargo publish -p krab_cli
cargo publish -p krab_orchestrator
```

`cargo publish --workspace` computes this order itself and is the supported
path; the explicit list is here so a partial publish can be resumed.

`krab_macros` goes first even though neither it nor `krab_core` depends on the
other's *published* form: they dev-depend on each other, and those
dev-dependencies are deliberately **path-only and version-less**
(`crates/framework/krab_core/Cargo.toml:156`,
`crates/framework/krab_macros/Cargo.toml:28`). Cargo strips version-less path
dev-dependencies on publish, which is the only reason the cycle is publishable
at all. Adding a version to either makes the pair unpublishable with no crate to
start from.

**Irreversible.** A published version cannot be replaced. `cargo yank` only
stops *new* resolutions; it does not remove the version.

Allow a minute between crates for the index to update, or the next crate will
fail to resolve its sibling.

**Done when** all five versions resolve on crates.io.

---

## Phase 7 — Tag and mirror

```sh
git tag -a v0.5.0 -m "0.5.0"
git push origin v0.5.0
git push public v0.5.0
```

> Before this release `git ls-remote --tags origin` was **empty**: `v0.2.0`,
> `v0.3.0` and `v0.4.0` existed locally and on `public` only, which is why
> `release-attestation.yaml` (`on: push: tags: 'v*'`) had never even been
> triggered. `v0.5.0` changed that — the trigger fired, and died at startup like
> every other run.
> Pushing to `origin` is still worth doing — the tag is the immutable source
> snapshot `RELEASE_POLICY.md` requires — but do **not** expect an attestation
> run to appear, and do not cite one.

**Done when** `git ls-remote --tags <remote>` shows `v0.5.0` on both remotes,
and the GitHub release is cut on `krab-pub`.

---

## Phase 8 — Verify the published artifacts, not just the build

This is the check `0.2.0` lacked, and the reason `0.4.0` existed. After
publishing `krab_client`, confirm the registry has the features the source
declares:

```sh
curl -s https://crates.io/api/v1/crates/krab_client/0.5.0 \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version']['features'])"
```

It must list `default`, `web`, `demo-islands`, and `debug`, and `default` must
contain `web`. If `default` is absent or lacks `web`, the inert stub shipped
again — yank immediately and cut a patch.

- [ ] `cargo install krab_cli` puts a binary named `krab` on `PATH`. The package
      cannot be named `krab`; the binary comes from an explicit `[[bin]]`.
- [ ] `krab doctor --strict` passes in a fresh `krab new` project — the exact
      path that was broken at `0.2.0`.
- [ ] `krab new` still builds for all five templates (`default`, `saas`,
      `edge-ssr`, `event-stream`, `fullstack`) against the *published* crates,
      not the workspace paths.
- [ ] Release notes carry the Required Release Artifacts from
      [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md): tag, changelog entry,
      evidence references (bundle paths, **not** CI links), rollback notes,
      known issues, SBOM.

---

## Open items that are not release blockers

- **GitHub Actions has never executed on push**, on either repository — 166 runs
  and 0 successes on `krab`, 111 and 0 on `krab-pub`, an account-level billing
  condition unchanged across three releases. Gate coverage comes from
  containerised runs. "Verified in a containerised run" and "enforced on push"
  are different claims; keep them apart in release evidence.
- `hydrate_recursive` is still undecomposed.
- The Required Release Artifacts table in
  [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) lists two artifacts this release
  cannot produce: **CI evidence** ("links to passing CI runs") and the **SBOM**,
  whose stated generator is the `dependency-security` workflow. No workflow has
  ever executed, so neither exists. Phase 8 substitutes evidence-bundle paths for
  the first; the second is simply absent. Either generate the SBOM outside CI or
  amend the policy — do not tick the row.
