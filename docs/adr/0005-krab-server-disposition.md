# ADR 0005: Disposition of `krab_server`

## Status

**Accepted** — 2026-08-08, by the repository owner.

Implemented in the same change. The sequencing in decision step 1 was honoured:
`resolve_static_pkg_path`, `static_mime_for`, and `normalize_static_mime` were
ported to [`krab_core::static_assets`](../../crates/framework/krab_core/src/static_assets.rs)
behind the `rest` feature, with their original tests plus four new cases
(absolute paths in both spellings, a `..` nested mid-path, and a missing file),
and verified green **before** `crates/framework/krab_server/` was removed.

## Context

`crates/framework/krab_server/src/lib.rs` is 502 lines providing a Hyper-based
`Server`, a trie `Router`, an `IntoResponse` trait, and static-asset serving
under `/pkg/`. [`docs/roadmap.md`](../roadmap.md) lists it under "frozen
architecture boundary": *"Frontend SSR/islands runtime remains on
`crates/framework/krab_server/src/lib.rs`."*

It does not run. Nothing calls it.

```
$ grep -rn "krab_server" --include=*.rs .
crates/framework/krab_server/src/lib.rs:463:  ...temp_dir().join("krab_server_static_root_reject")
crates/framework/krab_server/src/lib.rs:472:  ...temp_dir().join("krab_server_static_root_accept")
```

Two hits, both inside its own tests. The only other reference is the dependency
line in [`services/service_frontend/Cargo.toml`](../../services/service_frontend/Cargo.toml),
which pulls the crate in and never imports from it.

What actually serves the frontend is
[`services/service_frontend/build.rs`](../../services/service_frontend/build.rs),
which generates:

```rust
pub fn register_routes<S>(router: axum::Router<S>, state: S) -> axum::Router<S>
    router
        .route("/about", axum::routing::get(route_about::handler))
```

Axum, not `krab_server`. [`CLAUDE.md`](../../CLAUDE.md) states generated handler
signatures "must match `Router::add_route`" — they must in fact match
`axum::routing::get`. So the roadmap, `CLAUDE.md`, and the code disagree three
ways, and have since the crate was written.

### The crate is also substantially worse than what replaced it

Two defects, neither covered by its eight tests:

**No HTTP method routing.** `add_route(path, handler)` and `handle(path)` take
no method. `Method`, `GET`, and `POST` do not appear anywhere in the file. A
`GET` and a `POST` to the same path are indistinguishable — every route answers
every verb.

**The trie does not backtrack.** `handle` prefers a static child whenever one
matches and keeps no alternatives stack:

```rust
if let Some(node) = current.children.get(segment) {
    current = node;                        // committed; never reconsidered
} else if let Some((param_name, node)) = &current.dynamic_child {
```

With `/a/b` and `/:x/c` registered, `/a/c` consumes `a` down the static branch,
finds no `c` child and no `dynamic_child` on that node, and returns `None` — a
404 for a route that is registered. Axum's `matchit` router handles this
correctly, as does every mainstream router.

Fixing both is not hard. The question is why we would.

### What is worth keeping

One thing, and it is a security control rather than a routing feature:
`resolve_static_pkg_path` (lib.rs:358) canonicalises a requested static path and
rejects anything resolving outside the root, defending `/pkg/` against
traversal. It has two tests. `static_mime_for` and `normalize_static_mime`
alongside it handle content-type mapping with a safe
`application/octet-stream` default and `charset=utf-8` on HTML.

`tower_http::services::ServeDir` covers most of this, but the explicit
canonicalise-and-compare is worth preserving rather than re-deriving.

## Decision

**Delete `krab_server`. Adopt Axum as the stated SSR foundation, which is what
it has always been in practice.**

1. Port `resolve_static_pkg_path`, `static_mime_for`, and `normalize_static_mime`
   into `krab_core` behind the `rest` feature, with their existing tests, in a
   commit that lands **before** the deletion.
2. Remove the crate directory, its `[workspace] members` entry, and the
   `service_frontend` dependency.
3. Correct [`docs/roadmap.md`](../roadmap.md) and the `Router::add_route` claim
   in [`CLAUDE.md`](../../CLAUDE.md).
4. Annotate the frozen-boundary record in `internal/plans/fullstack_remediation.md`
   with a dated pointer here. Do not rewrite its history.

`krab_server` is unpublished, so no consumer can be broken by removing it.

## Consequences

**The documentation stops describing a runtime that does not run.** This is the
main gain. A reader following the roadmap to `krab_server/src/lib.rs` currently
lands on code that has never handled a request.

**One fewer crate to publish, version, and keep advisory-clean**, and one fewer
router to carry method routing, backtracking, wildcards, and precedence rules
into — all of which Axum already has, tested by a far larger user base.

**A future first-party server is not foreclosed.** If Krab later needs to own
its HTTP layer — to control streaming SSR or the hydration protocol at the
socket — that is a deliberate design task, not a resurrection of this code. The
git history remains.

**The static-path traversal defence must land first.** The sequencing in
decision step 1 is the whole risk of this ADR. Deleting before porting drops a
security control.

**`docs/architecture/design.md` and `service_composition.md` need a read-through**
for other references to `krab_server` as the serving path.

## Alternatives considered

**Retain it and make `service_frontend` actually use it.** This is the option
the roadmap's frozen boundary implies. It means adding method routing, adding
backtracking, and rewriting `build.rs` to emit `krab_server::Router`
registration — replacing a working Axum path with a less capable one, and taking
on permanent maintenance of a router with no differentiating behaviour.
Rejected: the cost is ongoing and the benefit is that a stale document becomes
true, which is more cheaply achieved by correcting the document.

**Keep it unused and mark it experimental.** Rejected. An unpublished,
uncalled, untested-in-anger crate that governance documents point at as the
production serving path is worse than either using it or removing it. It is the
current state, and it is what this ADR exists to end.

**Keep it as the public server abstraction and re-export Axum through it.** A
facade adds a version-coupled indirection layer over a stable, well-known API,
for no gain a user can name. Rejected.

## References

- Crate: `crates/framework/krab_server/src/lib.rs` (502 LOC)
- Actual serving path: [`services/service_frontend/build.rs`](../../services/service_frontend/build.rs)
- Contradicted claims: [`docs/roadmap.md`](../roadmap.md) item 4, [`CLAUDE.md`](../../CLAUDE.md) Known constraints
- Remediation plan: `internal/plans/framework_viability.md` Phase 3
