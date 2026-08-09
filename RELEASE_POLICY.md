# Release Policy

This document defines release channels, mandatory promotion criteria, versioning strategy, and required release artifacts.

---

## Release Channels

| Channel | Purpose | Audience |
|---|---|---|
| **Beta** (pre-production) | Integration and hardening releases | Internal teams, early adopters |
| **Stable** (production) | Releases approved for production use | All users |

---

## Stable Promotion Requirements

All conditions below must be satisfied before a beta release can be promoted to stable:

### CI Gates (all green)

| Workflow | Gate |
|---|---|
| `ops-hardening` | Formatting, linting, tests, `cargo-deny` |
| `dependency-security` | `cargo-audit` + SBOM generation |
| `api-contract` | API contract validation |
| `db-lifecycle` | Migration, rollback, drift checks |
| `e2e-depth` | Multi-service end-to-end testing |
| `nft` | Non-functional / load testing (when policy-triggered) |

### Security Requirements

- No unresolved high/critical dependency advisories.
- `cargo deny --all-features check advisories licenses bans` passes with policy-approved settings and no ignored advisories/licenses/sources exceptions.
- Production secret sourcing enforced (no inline secrets in non-dev environments).

### Operational Readiness

- SLO/burn-rate alert wiring validated.
- On-call runbook mapping validated (see [`docs/operations/oncall_playbook.md`](docs/operations/oncall_playbook.md)).
- Rollback rehearsal evidence exists for current migration version.

### Documentation

- Documentation reflects actual code state.

---

## Crate Publication

Krab is consumed from crates.io, not by cloning this repository. Six crates are
published; the four under `services/` carry `publish = false` and are reference
applications, not distributables.

### Published crates and order

Cargo will not accept a crate whose dependencies are not already on the index,
so publication is ordered by the dependency graph:

| Order | Crate | Depends on |
|---|---|---|
| 1 | `krab_macros` | — |
| 2 | `krab_core` | — |
| 3 | `krab_client` | `krab_core`, `krab_macros` |
| 4 | `krab_cli` | `krab_core` |
| 5 | `krab_orchestrator` | `krab_core` |

`cargo publish --workspace` computes this order itself and is the supported way
to release; the table exists so a human recovering from a partial publish knows
where to resume.

### Preconditions

- `cargo publish --workspace --dry-run` exits 0. This is enforced on every push
  by the `publish-dry-run` job in
  [`ops-hardening.yaml`](.github/workflows/ops-hardening.yaml) and is a
  promotion blocker, not an advisory check.
- The `version` values in `[workspace.dependencies]` match
  `[workspace.package] version` in the root [`Cargo.toml`](Cargo.toml).
  `scripts/check_workspace_layout.py` enforces this.
- Inter-crate **dev**-dependencies remain path-only and version-less.
  `krab_core` and `krab_macros` dev-depend on each other; Cargo strips
  version-less path dev-dependencies on publish, which is the only reason that
  cycle is publishable. Adding a version to either makes the pair unpublishable
  with no crate to start from.

### Binary name

`krab_cli` publishes a binary named `krab` via an explicit `[[bin]]` section.
The package cannot be named `krab` — that name was registered on crates.io in
2023 by an unrelated crate. `cargo install krab_cli` puts `krab` on `PATH`.

---

## Versioning Policy

- **Format**: Semantic Versioning — `MAJOR.MINOR.PATCH`
- **MAJOR**: Breaking API changes or incompatible architectural shifts
- **MINOR**: New features, new endpoints, backward-compatible changes
- **PATCH**: Bug fixes, security patches, documentation updates

### While the version is below `1.0`

The rule above describes a post-`1.0` crate and does **not** apply as written
while Krab is on `0.x`. Cargo treats the leftmost non-zero field as the
compatibility boundary, so for a `0.x.y` version the **minor** field plays the
role of major:

| Change | Post-`1.0` | On `0.x` |
|---|---|---|
| Breaking API change | `MAJOR` | **`MINOR`** — `0.1.z` → `0.2.0` |
| Backward-compatible feature | `MINOR` | `PATCH` — `0.2.0` → `0.2.1` |
| Bug fix, docs | `PATCH` | `PATCH` |

`^0.1` and `^0.2` are incompatible requirements; `0.1.1` and `0.1.2` are not.
Releasing a breaking change as a patch therefore breaks every downstream build
with nothing in the version to signal it.

Reaching `1.0` is a separate decision, gated on
[`docs/operations/production_readiness.md`](docs/operations/production_readiness.md),
not something a breaking change forces.

### Breaking Change Policy

Breaking API changes require:

1. Migration guidance in [`docs/reference/api.md`](docs/reference/api.md).
2. Deprecation notice in the previous release's [`CHANGELOG.md`](CHANGELOG.md).
3. Minimum one minor version with deprecation warning before removal.

---

## Required Release Artifacts

Every release publication must include:

| Artifact | Description |
|---|---|
| Git tag | Immutable source snapshot (e.g., `v0.1.0`) |
| CHANGELOG entry | Updated [`CHANGELOG.md`](CHANGELOG.md) with Added/Changed/Removed/Fixed/Security sections |
| CI evidence | Links to passing CI runs for all required gates |
| Rollback notes | Documented rollback procedure for the release |
| Known issues | List of known issues or limitations |
| SBOM | Software Bill of Materials generated by `dependency-security` workflow |

---

## Release Cadence

| Ceremony | Frequency | Purpose |
|---|---|---|
| Hardening release | Weekly | Merged improvements behind required gates |
| Stabilization review | Biweekly | Cross-functional review of regressions, risk, gate quality |
| Async triage | Daily | Owner updates on blockers, drift, incidents |

---

## Hotfix Process

For critical security or availability issues:

1. Branch from the latest stable tag: `hotfix/description`
2. Apply minimal fix with test coverage.
3. Run full CI gate suite.
4. Tag as `vX.Y.Z+1` patch release.
5. Update `CHANGELOG.md` with Security or Fixed section.
6. Cherry-pick fix back to `main`.
