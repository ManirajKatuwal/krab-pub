# ADR 0016: `<Suspense>` Boundaries

## Status

**Accepted** — 2026-09-30. Implemented in the same change, for release in
`0.6.0`.

Extends [ADR 0008](0008-view-control-flow-tags.md) with a third built-in
control-flow tag. Builds on [ADR 0009](0009-resource-ssr-semantics.md)
(resources render synchronously on the server) and
[ADR 0014](0014-context-api-and-owners.md) (context on a tree of owners).
[ADR 0017](0017-progressive-streaming-ssr.md) builds streaming on it.

## Context

`Resource` (ADR 0009) exposes `state()` and `value()` signals, and every page
that loads data wrote the same thing by hand:

```rust
<Show when={move || user.state().get().is_ready()} fallback={|| spinner()}>
```

once per resource, with the conditions combined manually when a section needs
several. There was also no place to hang streaming: the suspense marker
vocabulary (`<!--krab:suspense:{id}:pending|resolved-->`) existed in
`render_stream` with nothing in the view layer that produced it.

## Decision

**`<Suspense fallback={...}>children</Suspense>` is a built-in control-flow
tag** — reserved like `<Show>` and `<For>`, so a user component named
`Suspense` must be written with a path. `fallback` is required: a boundary
without one renders an empty hole while it loads, which is the thing the tag
exists to avoid. It expands to:

```rust
krab_core::suspense::suspense(fallback, || Node::Fragment(vec![children...]))
```

### Registration through context

`suspense` creates a `SuspenseContext`, then runs the children closure **once**,
inside `with_owner`, after `provide_context(context)`. Every `Resource` built
while the children are built finds it with `use_context` and registers a
tracked "is pending" source. The children are a closure rather than a value
precisely because of ADR 0014's consequence that component children are built
eagerly, *before* the parent's body runs: a value would be built outside the
boundary's scope and nothing inside it would register. `SuspenseContext::register`
is public, so a hand-rolled async value can hold a boundary too; `use_suspense()`
returns the nearest one.

Resources register at **creation**, not at read. Reads usually happen inside
`move ||` closures that are evaluated at render or mount time, outside every
scope, where no context is visible; creation happens in the component body,
inside it.

### Rendering

The boundary is a `Node::Dynamic`. Each evaluation reads every registered
source (so it subscribes to all of them) and returns

```text
Comment("krab:suspense:{id}:pending"), <fallback or children>, Comment("krab:suspense:{id}:resolved")
```

The children `Node` is built once and cloned into each render: rebuilding it
would re-create its resources, which would refetch, which would re-render —
a loop. So values must be read reactively inside the children (a `move ||`
interpolation), the same rule as for `Show`.

**"Pending" means no first value yet.** A resource registers
`state == Pending && value == None`. A refetch over existing data, or a
failure, does not return the boundary to its fallback: swapping loaded content
out for a spinner destroys the DOM (focus, selection, scroll) that the refresh
exists to update. A refetch indicator belongs inside the children, driven by
`state()`. (This is the behaviour other frameworks call a *transition*; the
initial-load-only rule makes it the default rather than a second tag.)

### Server

Per ADR 0009, synchronously: with an initial value a resource is `Ready` and the
children render; without one it is `Pending` and the fallback renders. The
output is final either way, and is wrapped `pending … resolved`, so
`is_finalized_ssr_snapshot` holds and the ISR cache stores it. The opening
marker delimits the boundary; the closing one says the server is done with it.
ADR 0017 adds the one exception — a deferred boundary under a streaming render.

### Comments in the tree: `Node::Comment`

The markers have to be *nodes*, not text spliced around the render: a
`<Suspense>` inside an island is hydrated, and the hydration walk matches DOM
children to vnodes by position — a comment it does not expect is a mismatch
that gets removed, shifting every sibling. So `Node` gains a variant,
`Node::Comment(String)`, rendered as `<!--text-->` with `--` neutralised.
`krab_client` hydrates it by adopting a DOM comment in its slot **without
comparing text** (boundary ids come from a per-process counter that the
server and the browser advance independently), creates it with
`document.createComment`, and patches it in place.

### Browser

The boundary is an ordinary dynamic region: when the last pending source
settles, the effect re-runs and the reconciler swaps fallback for children,
keeping the markers in place.

## Consequences

- **Breaking:** `Node` has a new variant. A `match` that listed every variant
  needs a `Node::Comment(_)` arm. (Inside the workspace: render, hydration
  annotation, the hydration walk and the reconciler.)
- `Suspense` is reserved in `view!`; a component of that name needs a path.
- Boundary ids (`s1`, `s2`, …) come from a process-wide counter, so two renders
  of the same page differ in their marker text. Pages with islands already
  differed in their boundary ids; ISR caches the rendered snapshot and does not
  compare renders, so this changes nothing there.
- Inside an island, a boundary behaves identically on both sides: the client's
  resource starts `Pending` without `initial` (and fetches), or `Ready` with it,
  exactly as the server rendered.

## Alternatives considered

**Registration on read.** Would catch a resource created outside the boundary
and read inside it, but reads happen in deferred closures outside every scope;
making it work would mean re-entering the boundary's owner around every
dynamic evaluation beneath it. Creation-time registration is predictable and
cheap; a resource created outside can be registered explicitly with
`use_suspense()` and `register`.

**Fallback on every refetch.** Rejected for the DOM-destruction reason above.

**Markers as text around the render (no `Node` variant).** Works for a page
shell but breaks hydration for any boundary inside an island.

**A wrapper element (`<krab-suspense>`) instead of comments.** Changes layout
and CSS selectors (`.list > li`), and the marker vocabulary the cache already
parses is comment-based.

## References

- `crates/framework/krab_core/src/suspense.rs` — `suspense`, `SuspenseContext`,
  `use_suspense`, tests
- `crates/framework/krab_core/src/resource.rs` — registration in `build`
- `crates/framework/krab_macros/src/view.rs` — `ControlFlow::Suspense`
- `crates/framework/krab_client/src/hydration.rs` — `hydrate_comment`
- Tests: `krab_macros` `tests/view_expansion.rs`, `tests/compile_fail/suspense_*`;
  `krab_client` `tests/suspense_browser.rs`
