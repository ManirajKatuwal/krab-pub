# ADR 0014: Context API on a Tree of Owners

## Status

**Accepted** — 2026-09-30. Implemented in the same change, for release in
`0.6.0`.

## Context

`docs/architecture/design.md` listed a "Context API (dependency injection for
deep trees)" under state management from the first draft. None existed: there
was no `provide_context`, no `use_context`, and nothing in `krab_core::signal`
that could scope a value to part of a tree. A component that needed the current
user, locale, or theme took it as a prop, and so did every component between it
and the page that had the value.

The reactive system did have one tree: **effect ownership**. An effect created
while another runs becomes its child (`EffectState::children`) and is disposed
when the parent re-runs. That tree cannot carry context by itself, for two
reasons:

1. **It does not exist on the server.** Without the `web` feature,
   `create_effect` does nothing, so SSR — which renders a component tree by
   calling functions synchronously — has no effects and therefore no tree.
   Context has to work there; per-request values (the session, the locale) are
   the most common thing to inject.
2. **It answers a different question.** Effect ownership decides what is
   *disposed* when; context needs "what was provided *above* me?". A component
   is not an effect and has no lifecycle, but it is exactly the unit a context
   is provided in.

[ADR 0013](0013-view-component-tags.md) adds component tags to `view!`, which
gives the macro a place to open a scope around each component call.

## Decision

**Add an owner tree for context lookup, separate from effect ownership, and a
`provide_context` / `use_context` pair on top of it.** All in
`krab_core::signal`:

```rust
pub fn provide_context<T: Clone + 'static>(value: T);
pub fn use_context<T: Clone + 'static>() -> Option<T>;
pub fn with_owner<T>(f: impl FnOnce() -> T) -> T;

#[derive(Clone)]
pub struct Owner { /* Rc<OwnerState> */ }
impl Owner {
    pub fn new() -> Self;                  // child of the current owner, or a root
    pub fn current() -> Option<Self>;
    pub fn with<T>(&self, f: impl FnOnce() -> T) -> T;
    pub fn dispose(&self);
}
```

### The owner tree

An owner holds a parent pointer (strong) and a small list of `(TypeId, value)`
pairs. A thread-local `CURRENT_OWNER` names the owner code is running under.

- `use_context::<T>()` starts at the current owner and walks up through parents
  until it finds a `T`. So an outer value is visible in every scope inside it,
  and an inner value of the same type **shadows** it for that subtree only.
- `provide_context(v)` stores `v` in the current owner, replacing an earlier
  value of the same type *in that owner*.
- `with_owner(f)` runs `f` under a new owner parented to the current one, and
  restores the previous owner afterwards — through a drop guard, so a panic
  caught by `error_boundary` does not leave later renders inside a dead scope.

### Where owners come from

- **Component tags.** `view!` expands `<Card .../>` to
  `with_owner(move || Card(props))` (props are built outside the closure, so `?`
  and moves in attribute expressions keep their meaning). A component's
  `provide_context` therefore reaches every component it renders and none of
  its siblings. Without a scope per component, a child providing a `Theme`
  would have overwritten its parent's for every later sibling.
- **Islands.** `#[island]` runs its body under `with_owner` in all three of its
  expansions — the SSR wrapper, the browser-side function, and the hydration
  factory. The factory matters most: hydration calls it outside any scope, and
  without one the same island would see contexts on the server and silently
  lose them in the browser.
- **Effects and memos.** Each gets an owner at creation, parented to the owner
  current at that moment, and runs under it *every* time it runs. This is what
  makes an effect that re-runs long after its component returned — triggered by
  a click, from outside every scope — still see the contexts provided around
  its creation. What an effect's body provides belongs to that run: it is
  cleared before the next run and dropped when the effect is disposed, the same
  lifetime `on_cleanup` callbacks have. A memo behaves the same way per
  recomputation.
- **Entry points.** An SSR handler opens the request's scope:
  `with_owner(|| { provide_context(session); view! { <App/> }.render() })`.

### No thread-wide fallback

`provide_context` with no current owner does nothing except log
`context_provided_without_owner` at `warn`. A thread-level root scope would be
more forgiving and is exactly wrong on a server: a Tokio worker thread renders
many requests in turn, and a session provided for one must never be visible to
the next. Failing closed, with a named event, costs one `with_owner` at the
entry point.

### Threading

Owners are `Rc`-based and thread-local, like signals and `krab_core::Node`
(which is already `!Send`). A context is visible only to code running on the
thread, inside the scope, that provided it. This is the same constraint the
rest of the reactive system has and needs no new rule: build and render a tree
inside one synchronous section, as `Node` already requires.

## Consequences

**Contexts work identically in SSR and in the browser** for code that runs in a
component body or an effect, which is where they are read.

**Read contexts in the body, not in deferred closures.** A `Node::Dynamic`
closure is evaluated at render (server) or mount (browser) time, after the
component returned and outside its scope; so is an event handler. Such code
should capture the value: `let theme = use_context::<Theme>();` in the body,
then `move ||` it in. A callback that must look up later can capture
`Owner::current()` and re-enter with `owner.with(...)`. Effects are the
exception, because they carry their owner.

**Children are built before their parent runs.** ADR 0013 passes children
eagerly as a `Node`, so `<ThemeProvider><Page/></ThemeProvider>` builds `Page`
*before* `ThemeProvider`'s body calls `provide_context` — the page does not see
the theme. A provider has to be an enclosing scope rather than a wrapper tag:
provide at the entry point or in the component that renders the consumer
directly. Lazily evaluated children (a `Children` closure type, as Leptos uses)
would lift this, at the price of turning every child expression into a
closure — changing the meaning of `?` and borrows in it — and is left for a
future ADR.

**Direct calls do not get a scope.** `{Card(props)}` runs in its caller's
scope; if `Card` provides a context, its caller's later siblings see it. Wrap
the call in `with_owner` when that matters, or use the tag.

**Memory.** An owner lives as long as anything refers to it: an effect or memo
created in the scope, a captured `Owner`, or the call stack. Parent pointers
are strong so that such a child can reach its ancestors' contexts; nothing
points down the tree, so owners cannot form a cycle among themselves. A context
value that captures its own `Owner` can, and would leak like any `Rc` cycle.

**Cost.** One `Rc` allocation per component call, effect, and memo; a lookup is
a walk up the tree comparing `TypeId`s, usually over empty lists.

## Alternatives considered

**Attach contexts to the effect tree.** Rejected: that tree does not exist in
SSR builds, and components are not effects.

**A thread-level default scope.** Rejected: cross-request leakage on servers
(see above).

**Keying by a user-supplied key instead of by type.** Rejected for now:
`TypeId` keying gives each context a compile-time identity for free, and a
newtype (`struct Locale(String)`) distinguishes two values of the same
underlying type. Every framework in the family does the same.

**Panicking `expect_context`.** Not added. Startup and render paths in this
workspace do not panic by convention; `use_context(...).unwrap_or_default()` or
an explicit `match` keeps the missing-provider case visible at the call site.

## References

- Implementation and tests: `crates/framework/krab_core/src/signal.rs`
  (`OwnerState`, `Owner`, `with_owner`, `provide_context`, `use_context`,
  `mod context_tests`)
- Scope per component: `crates/framework/krab_macros/src/view.rs`
  (`impl ToTokens for Component`), `crates/framework/krab_macros/src/island.rs`
- Threading constraints: [`docs/architecture/signal_safety.md`](../architecture/signal_safety.md)
