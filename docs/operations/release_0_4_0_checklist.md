# Release checklist — 0.4.0

The release that makes the published crates match the documented framework.

## Why 0.4.0, and why crates.io skips 0.3.0

`0.3.0` was tagged and released on GitHub on 2026-08-12 but **never published to
crates.io**. Three further bodies of work then landed on top of that tag — the
`krab_client` runtime fix, the `krab_macros`/`krab_orchestrator` audit, and the
`krab_cli` audit — so the tag no longer describes anything anyone can install.

Rather than move a tag that is already public, `0.4.0` supersedes it. The
registry therefore goes `0.2.0` → `0.4.0`, and `0.4.0` carries the `0.3.0`
changes as well. Consumers upgrading from `0.2.0` must read both changelog
sections.

This release is breaking, and on `0x` the **minor** field is the compatibility
boundary — `^0.2` and `^0.4` do not unify. See
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md#while-the-version-is-below-10).

Breaking changes carried by `0.4.0` (including those inherited from `0.3.0`):

| Change | Who it affects |
|---|---|
| `krab_client` defaults now include `web` | Anyone who built expecting the inert stub — see below |
| Protocol resolution runs after authentication | Anyone using `KRAB_PROTOCOL_TENANT_OVERRIDES_JSON` |
| Disabled route-family protocols return 400, not 404 | Any client asserting on 404 |
| `DistributedStore` requires `set_if_absent` | Anyone implementing the trait |
| Malformed auth/protocol policy JSON fails closed | Deployments with invalid policy JSON that previously booted |
| `krab release certify --out` default changed | CI that relied on the old `release-evidence` default |
| `krab doctor` reports `SKIP` for inapplicable checks | Anyone parsing doctor output positionally |

Migration steps are in
[`docs/guides/migration_guide.md`](../guides/migration_guide.md).

## The defect this release exists to fix

`krab_client` `0.1.x` and `0.2.0` were published **without the `web` feature**.
`web` gates the entire hydration runtime, so the crate on crates.io was a ~15 KB
module whose `hydrate()` logged one line and returned. Every consumer that
installed it from the registry had islands that never came alive.

`web` is now a default. The check that would have caught this is in "Verify
before publishing" below, and it inspects the **published** artifact rather than
the local build.

## Done

- [x] Private `main` and `krab-pub` reconciled — both carry the `krab_client`,
      `krab_macros`/`krab_orchestrator`, and `krab_cli` audits.
- [x] Workspace version bumped to `0.4.0`, including the three duplicated pins in
      `[workspace.dependencies]` and `Cargo.lock`.
- [x] `CHANGELOG.md` has a `[0.4.0]` heading, with the `0.3.0`-never-published
      note recorded in it.
- [x] Docs corrected: `CLAUDE.md` version and the `demo-islands` removal version
      (`0.6.0`, not `0.5.0`), `docs/README.md` release row, `README.md`
      installation claim, `docs/reference/database.md` version pins and
      deprecation window.

## Verify before publishing

Run on a settled tree with no other `cargo` process holding the build lock.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p krab_core --all-features
cargo test -p krab_macros
cargo doc --workspace --no-deps
cargo run -p krab_cli -- security dependency-gate --diagnostics
python3 scripts/check_workspace_layout.py
cargo publish --workspace --dry-run
```

`cargo test --workspace` alone proves very little: `krab_core` has no default
features, so it compiles a fraction of the surface. The `--all-features` run is
the one that matters.

And the two that need a browser:

```sh
cargo install wasm-bindgen-cli --version 0.2.127 --locked
CHROMEDRIVER=/path/to/chromedriver \
  cargo test -p krab_client --target wasm32-unknown-unknown --features web

wasm-pack build examples/reference_apps/islands_rpc --target web -- --features web
```

### Verify the published artifact, not just the build

This is the check `0.2.0` lacked. After publishing `krab_client`, confirm the
registry has the features the source declares:

```sh
curl -s https://crates.io/api/v1/crates/krab_client/0.4.0 \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['version']['features'])"
```

It must list `default`, `web`, `demo-islands`, and `debug`. If `default` is
absent or does not contain `web`, the stub shipped again — yank immediately and
cut a patch.

## Publish

Dependency order. `cargo publish --workspace` computes this itself and is the
supported path; the explicit list is here so a partial publish can be resumed.

```sh
cargo publish -p krab_macros
cargo publish -p krab_core
cargo publish -p krab_client
cargo publish -p krab_cli
cargo publish -p krab_orchestrator
```

**Irreversible.** A published version cannot be replaced. `cargo yank` only
stops *new* resolutions; it does not remove the version.

Allow a minute between crates for the index to update, or the next crate will
fail to resolve its sibling.

## After publishing

```sh
git tag -a v0.4.0 -m "0.4.0"
git push origin v0.4.0
git push public v0.4.0
```

- [ ] Confirm `cargo install krab_cli` puts a binary named `krab` on `PATH`, and
      that `krab doctor --strict` passes in a fresh `krab new` project — the
      exact path that was broken at `0.2.0`.
- [ ] Confirm the `krab_client` published feature set per the check above.
- [ ] Cut the GitHub release on `krab-pub`, and note in the `v0.3.0` release that
      it was superseded without reaching crates.io.
- [ ] Attach evidence links per the Required Release Artifacts table in
      [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md).

## Open items that are not release blockers

- **GitHub Actions has never executed on push.** Every run across both repos
  fails at startup with zero jobs, an account-level billing condition. Gate
  coverage for this release comes from containerised runs, not from CI. Treat
  "verified in a containerised run" and "enforced on push" as different claims.
- `hydrate_recursive` is still undecomposed.
