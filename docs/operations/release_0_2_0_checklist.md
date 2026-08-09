# Release checklist — 0.2.0

Prepared, **not published.** Everything below up to "Publish" has been done and
verified; the publish step is yours to run because it cannot be undone.

## Why 0.2.0 rather than 0.1.2

This release is breaking. Cargo treats the leftmost non-zero version field as
the compatibility boundary, so on `0.x` the **minor** field is what signals an
incompatible change — `^0.1` and `^0.2` do not unify, `0.1.1` and `0.1.2` do.
Shipping these as a patch would break every downstream `^0.1` build with nothing
in the version to indicate it. See
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md#while-the-version-is-below-10).

Breaking changes in this release:

| Change | Who it affects |
|---|---|
| `IsrCache` methods are `async` and return `Result` | Anyone using ISR |
| `IsrEntry::generated_at` is `SystemTime`, not `Instant` | Anyone reading it directly |
| `DistributedStore` requires `delete` + `keys_with_prefix` | Anyone implementing the trait |
| `ProtocolKind::parse("grpc")` returns `None` | Anyone with `KRAB_PROTOCOL_ENABLED=grpc` |
| Credentials must be Argon2id PHC hashes | Every non-local `service_auth` deployment |
| `krab_server` removed | Nobody — it was never published |
| `grpc` → `grpc-semantics` | Nobody yet — aliased for one minor version |

Migration steps for each are in
[`docs/guides/migration_guide.md`](../guides/migration_guide.md).

## Done

- [x] Workspace version bumped to `0.2.0`, including the duplicated pins in
      `[workspace.dependencies]` (they are checked by
      `scripts/check_workspace_layout.py`, which passes).
- [x] Deprecation windows corrected: aliases deprecated *in* `0.2.0` are
      removable no earlier than `0.3.0`, not `0.2.0`.
- [x] `CHANGELOG.md` has a `[0.2.0]` heading with Security / Removed / Changed /
      Fixed / Added sections.
- [x] `RELEASE_POLICY.md` documents the `0.x` versioning rule, which it did not
      previously cover.
- [x] Migration guidance written for every breaking change.
- [x] Five publishable crates confirmed (`krab_core`, `krab_macros`,
      `krab_client`, `krab_cli`, `krab_orchestrator`); the four `services/*`
      crates and the reference app are `publish = false`.

## Verify before publishing

Run these on a clean checkout, with no other `cargo` process competing for the
build lock — a gate run against an unsettled tree produces failures that are
artifacts rather than defects.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p krab_core --all-features
cargo doc --workspace --no-deps
cargo run -p krab_cli -- security dependency-gate --diagnostics
python3 scripts/check_workspace_layout.py
cargo publish --workspace --dry-run
```

And the two that need a browser, which cannot run on a machine without
`wasm-pack` and headless Chrome — CI covers both:

```sh
wasm-pack test --headless --chrome crates/framework/krab_client -- --features web
wasm-pack build examples/reference_apps/islands_rpc --target web -- --features web
```

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

**Irreversible.** A published version cannot be replaced, and a crate name
cannot be released once claimed. `cargo yank` only stops *new* resolutions; it
does not remove the version.

Allow a minute between crates for the index to update, or the next crate will
fail to resolve its sibling.

## After publishing

```sh
git tag -a v0.2.0 -m "0.2.0"
git push origin v0.2.0
```

- [ ] Move the `[0.2.0]` heading's "unreleased, prepared" note to a date.
- [ ] Drop the "Not published yet" callouts in
      [`README.md`](../../README.md) and
      [`docs/guides/getting_started.md`](../guides/getting_started.md) — both
      currently tell readers to install from a checkout.
- [ ] Confirm `cargo install krab_cli` puts a binary named `krab` on `PATH`.
- [ ] Attach CI evidence links per the Required Release Artifacts table in
      [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md).

## Open items that are not release blockers

- **Phase 6 (credential verification) has not had its second reviewer.** The
  plan requires one for a security boundary. The code is implemented and tested;
  the sign-off is outstanding. Publishing before it is a judgement call — the
  new code is strictly safer than the plaintext comparison it replaced, but the
  governance requirement is unmet either way.
- `hydrate_recursive` (324 lines) is not yet decomposed; the browser test
  harness that would make that safe has not run anywhere yet.
