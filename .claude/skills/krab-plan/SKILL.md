---
name: krab-plan
description: Author, review, close, supersede, or abandon a plan document in the Krab repository under internal/plans/. Use when asked to write a plan, roadmap, phase breakdown, or remediation plan; when marking a plan phase or plan complete; or when auditing whether a plan's claims are actually substantiated. Enforces internal/plans/PLAN_CREATION_RULES.md and internal/plans/PLAN_CLOSING_RULES.md, including evidence linkage and governance propagation.
---

# Krab plan lifecycle

Plans in this repository are governance artifacts, not notes. A closed plan is a
claim consumed by release promotion decisions. Treat both authoring and closing
as adjudicable acts.

Authoritative rules:
- [`internal/plans/PLAN_CREATION_RULES.md`](../../../internal/plans/PLAN_CREATION_RULES.md)
- [`internal/plans/PLAN_CLOSING_RULES.md`](../../../internal/plans/PLAN_CLOSING_RULES.md)

Read the relevant one before acting. This skill routes and enforces; those files
are the specification.

---

## Creating a plan

### Is one needed?

Required if the work touches more than one crate/service, a public API or wire
contract, DB schema or migration governance, a security boundary, a CI gate,
takes more than a day, or reverses a prior plan decision. Otherwise: land the
change with a `CHANGELOG.md` entry, no plan.

### Before writing

Read the current state — do not infer it. Every current-state claim needs a
`path/file.rs:line` reference or a command that demonstrates it. If you cannot
establish it, the first task in the plan is to establish it, marked
`ASSUMPTION:`.

### Placement

- `internal/plans/NN_topic.md` — architecture/roadmap
- `internal/plans/<topic>.md` — single initiative
- `internal/plans/<topic>/` with `README.md` + `MASTER_TASK_LIST.md` + `NN_phase_*.md` —
  multi-phase (mirror [`internal/plans/protocol_flexibility/`](../../../internal/plans/protocol_flexibility))

Link the new plan from [`internal/plans/README.md`](../../../internal/plans/README.md) in the same
commit. Unlinked plans drift.

### Required sections, in order

Front matter table (Status, Owner, Created, Last reviewed, Target, Supersedes,
Evidence), then: Objective · Current state · Scope (in **and** out) · Phases and
tasks · Verification plan · Risks and rollback · Governance impact · Exit criteria.

None are optional. Section-by-section requirements are in the creation rules.

### Quality bar

- Objective states observable system behaviour, not activity
- Every phase has a status marker and a **Done when** line
- Verification commands are copy-pasteable and carry the feature flags CI uses
  (`cargo test -p krab_core --features rest protocol`, not bare `cargo test`)
- Governance impact answers every category explicitly, "none" included
- Absolute dates only
- Exit criteria adjudicable by someone who did not write the plan

---

## Closing a plan

Never mark a plan or phase complete on the basis of "the work looks done".

### Sequence

1. **Re-read the plan's exit criteria.** Adjudicate each one individually.
2. **Run the plan's verification commands at the closing commit.** Use the
   `krab-verify` skill. Evidence from an earlier commit does not attest this one.
3. **Log every run** in
   [`internal/audit/VERIFICATION_EVIDENCE_LOG.md`](../../../internal/audit/VERIFICATION_EVIDENCE_LOG.md).
4. **Work the closing checklist** in `PLAN_CLOSING_RULES.md` §3: completion,
   evidence, gates, governance propagation, changelog, residue.
5. **Append a Closing Record** using the template in §4 — closed on, closing
   commit, closed by, reviewed by, evidence links, Delivered / Not delivered /
   Carried forward / Deviations / Verification summary.
6. **Update front matter** (`Status: Closed`, `Last reviewed`, `Evidence`) and
   the entry in [`internal/plans/README.md`](../../../internal/plans/README.md).

### Second reviewer required

If the plan touched a security boundary, DB schema or migration governance, a
released API or wire contract, or a CI gate definition — closure needs a second
reviewer, recorded by handle. Otherwise self-closure is permitted.

### Terminal statuses

`Closed` (criteria met and evidenced) · `Superseded` (§5) · `Abandoned` (§6).
There is no "mostly done" — either close a split-out completed portion and carry
the rest into a new plan, or leave the plan `Active`.

### Superseding and abandoning

- Superseding: both plans updated in the **same commit**; incomplete scope mapped
  to the new plan's phases under **Carried forward**; existing evidence re-linked.
- Abandoning: dated rationale; explicit disposition of partial work; any
  half-applied governance change reverted or documented. The repository must not
  be left describing behaviour that does not exist.

### Re-opening

Only when the plan's own exit criteria are found violated — new scope gets a new
plan. Set the affected phase to `⚠️ Re-opened`, append a dated Re-opening Record,
leave the Closing Record intact, and mark invalidated evidence rows as superseded
rather than deleting them.

---

## Auditing a plan's claims

When asked whether a plan is really done, check in this order and report gaps
rather than assuming good faith:

1. Do the phase markers agree with the front-matter `Status`?
2. Does every exit criterion have evidence, or only an assertion?
3. Does the `Evidence` field resolve to real ledger rows and real artifacts?
4. Were those runs at or after the closing commit?
5. Did the governance propagation actually land — grep `.env.example`,
   `docs/reference/environment.md`, `docs/reference/api.md`, the workflow files, `docs/adr/`?
6. Is there a `CHANGELOG.md` entry for each user-visible outcome?

---

## Non-negotiables

1. Evidence precedes closure. Run first, close second.
2. Unrun is not passed. A command not executed is logged `not run` with a reason.
3. Stale evidence does not count.
4. Green CI and the plan's own verification are both required where both apply.
5. Documentation drift within the plan's governance scope blocks closure.
6. A closed plan is reconstructible from its Closing Record alone: what was
   verified, at which commit, by whom, and where the artifacts are.

## Repository notes

- All internal documentation lives under `internal/` (`plans/`, `audit/`,
  `wiki/`, `reports/`), gitignored by a single rule. Plan files are local
  artifacts. The public-facing surface is `README.md`, `CONTRIBUTING.md`,
  `CHANGELOG.md`, `RELEASE_POLICY.md`, and everything under `docs/` — a plan
  must never be the only place a public behaviour is documented.
- Existing plans predate these rules and do not carry the front-matter block. Do
  not retrofit them wholesale — bring a plan up to standard when you next touch
  it substantively, and note the gap otherwise.
