<!-- One logical objective per PR. Branch prefix: feat/ fix/ chore/ docs/ refactor/ security/ -->

## What this changes

<!-- Outcome, not task list. Link the issue if one exists. -->

## Verification

<!-- All gates below are required before review (see CONTRIBUTING.md). -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo run -p krab_cli -- security dependency-gate --diagnostics`
- [ ] `cargo doc --workspace --no-deps`
- [ ] WASM affected? `wasm-pack build crates/framework/krab_client --release --target web`

## Change discipline

- [ ] `CHANGELOG.md` entry under `## [Unreleased]` (any user-visible change)
- [ ] New config knob documented in `.env.example` **and** `docs/reference/environment.md`
- [ ] New/changed endpoint reflected in `docs/reference/api.md`
- [ ] Breaking change: migration notes in `docs/reference/api.md` + `CHANGELOG.md`
- [ ] Migration includes `rollback_sql` (or is documented as irreversible)
- [ ] Not applicable — none of the above touched
