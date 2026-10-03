# Release checklist — 0.6.0

The removals-and-hardening release. Runbook: work the phases in order, and
treat a phase's "done when" as the only thing that closes it. It follows the
shape of the [0.5.0 checklist](release_0_5_0_checklist.md); where a step is
unchanged this document says so and links there instead of restating it.

## Why 0.6.0

0.6.0 removes everything that carried a removal version, and changes behaviour in
places a running deployment can notice. On `0.x` the **minor** field is the
compatibility boundary — `^0.5` and `^0.6` do not unify. See
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md#while-the-version-is-below-10).

| Change | Who it affects |
|---|---|
| `krab_client` feature `demo-islands` and `krab_client::components` removed | Anyone rendering `Counter` / `Toggle` / `Likes` from the framework — define them in your own crate |
| `run_migrations`, `SuspenseMarker`, `KrabConfig::from_env`, `WsRoom::connect/disconnect`, the `db`/`grpc` feature aliases and the `krab_core::grpc` module alias removed | Code still on a name deprecated since 0.2.0–0.5.0 |
| `krab db lifecycle|rollback|drift` fail without Postgres | Anyone who ran them where no database exists — they used to "pass" |
| Framework-only governance commands refuse outside the framework checkout | Generated projects that ran `krab contract check`, `krab release check`, … — they never worked there |
| Default tooling artifact root is `.krab/`, not `internal/audit/` | Scripts reading evidence from `internal/audit/` in a checkout **without** that directory |
| Latency histogram renamed to `krab_http_request_duration_seconds` (old name still emitted, buckets only) | Dashboards and recording rules — move before 0.7.0 |
| Auth-failure limiter store outage answers 503, not 429 | Alerting keyed on 429 during store incidents |
| `MemoryStore::incr` errors on a non-numeric value | Code sharing a key between a counter and something else |
| `RuntimeState` and `JwtProviderConfig` gained public fields | Anyone constructing them with struct literals |
| `Attribute` gained a public field (`dynamic`), `Node` a variant (`Comment`), and `Suspense` is a reserved `view!` tag | `Attribute { .. }` struct literals, exhaustive `match`es on `Node`, a component named `Suspense` |
Deprecated in this release and removed in `0.7.0`: `init_tracing(name)`, the
`krab_request_duration_seconds` metric name, the application routes on the
default open-path list (`KRAB_AUTH_LEGACY_OPEN_PATHS`), the `internal/audit/`
artifact fallback, `krab_core::image`, `krab_core::style_scope`, and the telemetry
model structs. `next_version` must therefore move to `0.7.0` — see Phase 1.

The full per-item list is [`docs/reference/api.md` §8](../reference/api.md#8-versioning-policy)
and the upgrade steps are the
[0.5.0 → 0.6.0 section of the migration guide](../guides/migration_guide.md#050--060).

## What gates this release, and what does not

Unchanged from 0.5.0: **no GitHub Actions run has ever succeeded on either
repository**, so every gate is a containerised or host run recorded in the
evidence ledger, never a CI link. See the
[0.5.0 section](release_0_5_0_checklist.md#what-gates-this-release-and-what-does-not)
for the full reasoning; it still applies word for word.

What *has* changed: the Windows host now builds and tests the whole workspace,
C dependencies included (`ring`, `libsqlite3-sys`) — verified 2026-09-30 with
`cargo test --workspace` and `cargo test -p krab_core --all-features`. The
0.5.0 checklist's statement that the host cannot link them is out of date. The
publish dry-run still belongs in the container (Phase 4), because it needs an
LF checkout.

---

> The release commit dated the CHANGELOG `2026-09-30`; it was re-dated
> `2026-10-01` when late audit fixes landed. If publishing happens on a later
> day, change the `## [0.6.0] — <date>` heading again first.

## Phase 1 — Freeze the version surface

Same four places as [0.5.0 §1.1](release_0_5_0_checklist.md#11-bump-the-workspace-version):
`[workspace.package] version`, the three `[workspace.dependencies]` pins
(`krab_core`, `krab_macros`, `krab_client`), and `Cargo.lock`. Set them to
`0.6.0`.

**Move `[workspace.metadata.krab] next_version` to `0.7.0` in the same commit.**
The mechanism is [0.5.0 §1.2](release_0_5_0_checklist.md#12-the-next_version-trap);
this release deprecates items with `since = "0.6.0"`, so the ceiling check
matters as well as the equality check.

```sh
python3 scripts/check_workspace_layout.py
```

**Done when** it prints `OK: workspace layout checks passed (11 members)` — one
more than 0.5.0, for `services/service_frontend_islands` — and exits 0.

---

## Phase 2 — Changelog, docs, and migration guidance

### 2.1 Changelog

Move the `[Unreleased]` body under `## [0.6.0] — <date>`, leave `[Unreleased]`
present and empty. Write a preamble naming the breaking rows of the table above
and linking the migration guide.

**Done when** `grep -n "^## \[" CHANGELOG.md` shows `[Unreleased]` immediately
above `[0.6.0]`.

### 2.2 Breaking-change policy

`RELEASE_POLICY.md` asks for migration guidance, a deprecation notice in the
previous release, and one minor of deprecation warning before removal.

| Change | Deprecated in | Policy |
|---|---|---|
| The seven removals | 0.2.0–0.5.0, each with `#[deprecated]` and a removal version | **Met** |
| `db lifecycle|rollback|drift` need Postgres | — | **Deviation, recorded.** A governance gate that cannot fail is a defect, not an API; there is nothing to deprecate |
| Governance commands refuse outside the framework | — | **Deviation, recorded.** They never worked there; they now say so |
| `.krab/` artifact root | — | **Met in spirit**: an existing `internal/audit/` keeps being used, with a warning, until 0.7.0 |
| Limiter 503, `incr` error, new public fields | — | **Deviation, recorded.** Each fixes a defect (a misleading status, silent data loss, missing metrics); the migration guide names each |
| Default open-path app routes | 0.6.0 | **Met**: deprecated now, removed in 0.7.0 |

**Done when** the `[0.6.0]` preamble states the deviations, as 0.5.0 did.

### 2.3 Version references

Re-check the install snippets [0.5.0 §2.3](release_0_5_0_checklist.md#23-version-references-users-will-actually-read)
listed — five of them, all saying `0.5` — plus `CLAUDE.md` (`- Version:`),
`README.md`, `docs/reference/database.md`.

The full list, collected on 2026-09-30 from the pre-release docs pass:

| File | What to change |
|---|---|
| `Cargo.toml` | `[workspace.package] version`, the `krab_core`/`krab_macros`/`krab_client` pins → `0.6.0`; `next_version` → `0.7.0`; then `Cargo.lock` |
| `CLAUDE.md` | the `- Version:` bullet (version, checklist link, "0.6.0 is in preparation") |
| `README.md` | "current release is `0.5.0`"; turn the "Upgrading to 0.5.0?" box and the "Next: 0.6.0" note into one "Upgrading to 0.6.0" box |
| `docs/README.md` | this checklist becomes "the current release"; demote the 0.5.0 row |
| `SECURITY.md` | supported `0.5.x` / `< 0.5` → `0.6.x` / `< 0.6` |
| `crates/framework/krab_client/README.md` | install snippets `"0.5"` (two) |
| `crates/framework/krab_core/README.md` | install snippet `"0.5"` |
| `crates/framework/krab_macros/README.md` | install snippet `"0.5"` |
| `docs/guides/getting_started.md` | install snippets `"0.5"` |
| `docs/reference/database.md` | two `"0.5.0"` pins |

Not version pins, leave alone: `tower = "0.5"`, `argon2 = "0.5"`, `le="0.5"` histogram buckets, and the "As of 0.5.0" history notes in `api.md`.

**Done when** no tracked file outside `CHANGELOG.md` and the release checklists
claims `0.5.0` is current.

### 2.4 Index this document

Add this checklist to [`docs/README.md`](../README.md) as the current release and
demote the 0.5.0 row.

---

## Phase 3 — Run the gate surface

The [0.5.0 gate set](release_0_5_0_checklist.md#phase-3--run-the-gate-surface)
plus what 0.6.0 added. Host or container; record which.

```sh
python3 scripts/check_workspace_layout.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p krab_core --all-features --all-targets -- -D warnings
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

New in 0.6.0:

```sh
# RedisStore against a real server (disposable container)
docker run -d --rm --name krab-test-redis -p 16379:6379 redis:7-alpine
KRAB_TEST_REDIS_URL=redis://127.0.0.1:16379/15 KRAB_REQUIRE_REDIS_TESTS=1 \
  cargo test -p krab_core --all-features --lib redis_store_tests

# wasm32 halves
cargo clippy -p krab_client --features web --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo clippy -p service_frontend_islands --features web --target wasm32-unknown-unknown --lib -- -D warnings
cargo clippy -p reference_app_islands_rpc --target wasm32-unknown-unknown --features web --lib -- -D warnings
wasm-pack build services/service_frontend_islands --release --target web --out-dir ../../dist/pkg -- --features web
wasm-pack build crates/framework/krab_client --release --target web -- --features web
grep -qa data-krab-boundary-state crates/framework/krab_client/pkg/krab_client_bg.wasm
```

The Postgres commands, the `doctor`/`env-check` pair, and the browser suites are
as in 0.5.0, except that the sidecar's IP is not stable — it was `172.17.0.4`
on 2026-09-30, not the `.3` the 0.5.0 checklist gives. Read it with
`docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' krab-ci-postgres`
before exporting `KRAB_TEST_DATABASE_URL`. There is no need to export
`GITHUB_SHA`: rehearsal and certify now record the local `HEAD` themselves. One
simplification: `doctor --strict` and `env-check --strict`
now load `./.env`, so in a generated project `cp .env.example .env` is enough.

**Done when** every command has exit 0 captured under the evidence bundle and a
ledger row records the SHA.

---

## Phase 4 — The publish dry-run, on an LF checkout

Unchanged: [0.5.0 Phase 4](release_0_5_0_checklist.md#phase-4--the-publish-dry-run-on-an-lf-checkout).
Clone inside `krab-ci`, confirm `git status --porcelain` is empty, run
`cargo publish --workspace --dry-run`.

Run it on the release commit, **after** Phase 1 has bumped every version to
`0.6.0`. The 0.5.0 checklist's reasoning — that the workspace form "resolves
siblings against the locally packaged versions" — holds only while the version
is unpublished. At `0.5.0`, which is already on crates.io, the dry-run verifies
each dependent against the *published* `krab_core`/`krab_macros`/`krab_client`
rather than the local tree, so it can pass while the release is broken. Confirm
`cargo metadata --no-deps --format-version 1` reports `0.6.0` in the clone
before running it.

**Done when** the clone is clean, it is at `0.6.0`, and the dry-run exits 0.

---

## Phase 5 — Governance evidence

As [0.5.0 Phase 5](release_0_5_0_checklist.md#phase-5--governance-evidence), with
two differences:

- `krab release certify` defaults its output under the artifact root. Pass
  `--out internal/audit/release-certify/release-0.6.0` explicitly (or set
  `KRAB_ARTIFACT_DIR=internal/audit`) so the bundle lands with the others.
- `krab db lifecycle`, `db rollback` and `db drift` now require Postgres too,
  not only `db rehearsal`; start the sidecar before any of them.

**Done when** `summary.json` has `"success": true`, or the failing step is in the
ledger with its artifact.

---

## Phase 6 — Publish

Same five crates, same order, same irreversibility as
[0.5.0 Phase 6](release_0_5_0_checklist.md#phase-6--publish). The workspace now
has six unpublished members: the five 0.5.0 listed plus
`services/service_frontend_islands` (`publish = false`).

**Requires the maintainer's registry credential and explicit go-ahead.**

---

## Phase 7 — Tag and mirror

As [0.5.0 Phase 7](release_0_5_0_checklist.md#phase-7--tag-and-mirror): tag
`v0.6.0`, build the public commit with `git commit-tree` on `public/main`,
confirm the trees are identical, push **after** crates.io — **with one
correction to the tag push.**

**Do not run `git push public v0.6.0`.** That pushes the *development* tag,
which points at a development-history commit, and uploads the whole private
history to the public repository. It is what the 0.5.0 procedure said, and it is
why `krab-pub`'s `v0.5.0` resolves to `85dee82`, a development commit, while
`v0.2.0`–`v0.4.0` resolve to public squash commits. Tag the public commit
instead and push that tag under the release name:

```sh
git tag -a public/v0.6.0 publish/0.6.0 -m "Krab 0.6.0"
git push public publish/0.6.0:main
git push public refs/tags/public/v0.6.0:refs/tags/v0.6.0
```

`internal/ci_local/mirror_0_6_0.sh` builds `publish/0.6.0`, verifies the tree
and the fast-forward, creates `public/v0.6.0`, and prints exactly these
commands.

**Done when** `git ls-remote public 'refs/tags/v0.6.0^{}'` resolves to a commit
that `git merge-base --is-ancestor <it> public/main` accepts.

---

## Phase 8 — Verify the published artifacts

```sh
curl -s https://crates.io/api/v1/crates/krab_client/0.6.0 \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version']['features'])"
```

It must list `default`, `web` and `debug` — and **not** `demo-islands` — with
`default` containing `web`.

- [ ] `cargo install krab_cli` puts `krab` on `PATH`.
- [ ] In a fresh `krab new` project, `cp .env.example .env` and then
      `krab doctor --strict` and `krab env-check --strict` pass **bare** — the
      first release where they do.
- [ ] `krab contract check` in that project exits non-zero with the
      "validates the Krab framework's own reference services" message.
- [ ] `krab new` builds for all five templates, and `krab gen service` output
      builds, against the *published* crates.
- [ ] Release notes carry the Required Release Artifacts, with evidence-bundle
      paths instead of CI links.

---

## Open items that are not release blockers

- GitHub Actions still has never executed on push (billing). Unchanged.
- The SBOM row of the Required Release Artifacts table still has no producer.
