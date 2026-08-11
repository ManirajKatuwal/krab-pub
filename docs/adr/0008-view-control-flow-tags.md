# ADR 0008: Control Flow in `view!` via a Fixed Set of Built-in Tags

## Status

**Accepted** — 2026-08-10. Implemented in the same change.

Refines [ADR 0006](0006-view-component-composition.md). It does not reverse it.

## Context

`view!` has no conditionals and no iteration. A list is written as an
interpolated closure returning a `Fragment`:

```rust
view! { <ul>{move || items.get().into_iter().map(row).collect::<Fragment>()}</ul> }
```

Until keyed reconciliation landed, that rebuilt the entire subtree on every
change, discarding DOM identity, focus, and scroll position. Reconciliation
fixed the *cost*, but the ergonomics are still poor and, more importantly, the
keys have to be stamped by hand for reconciliation to do anything useful. A user
who does not know `data-krab-node-id` exists gets positional matching and the
old behaviour.

### The collision with ADR 0006

ADR 0006 decided that **a capitalised tag name is a compile error**, because
`view!` emits tag names verbatim and `<MyComponent/>` would silently become
literal `<MyComponent>` markup that no browser renders.

`<Show>` and `<For>` are capitalised tags. So the obvious spelling — the one
Leptos, Dioxus, and SolidJS all use — is currently rejected.

This is worth being precise about, because it decides whether this ADR conflicts
with 0006 or extends it. **ADR 0006's reason was silent breakage, not
capitalisation.** Its own words: "it would emit the literal markup
`<MyComponent>`, which no browser renders and no test catches. Failing loudly
beats silently producing broken HTML."

A *fixed, closed* set of names the macro recognises explicitly has no silent
breakage risk. The macro either knows the name and expands it into a call, or it
does not and errors exactly as before. Nothing can be emitted as an unintended
literal tag.

## Decision

**`view!` recognises a closed set of built-in control-flow tags: `<Show>` and
`<For>`. Every other capitalised tag remains a compile error.**

Both expand to calls into `krab_core`, not to elements.

```rust
view! {
    <Show
        when={move || logged_in.get()}
        fallback={|| view! { <a href="/login">"Sign in"</a> }}
    >
        <p>"Welcome back"</p>
    </Show>
}
```

```rust
view! {
    <ul>
        <For
            each={move || todos.get()}
            key={|todo: &Todo| todo.id}
            view={|todo: Todo| view! { <li>{todo.title}</li> }}
        />
    </ul>
}
```

### Why attributes rather than a binding syntax

`<For>` takes its row renderer as a `view={…}` closure rather than introducing
`let:item` binding syntax. A new binding form is a language surface — scoping
rules, type inference, diagnostics — and this achieves the same result with
closures the compiler already understands. `let:item` remains available later
as sugar over exactly this.

### Why `<For>` owns key stamping

`key` is not decoration. `For` stamps the computed key onto the rendered node as
`data-krab-node-id`, which is the attribute the reconciler in
`krab_client::patch_children` matches on. That is the entire reason `<For>` is
worth having over a hand-written `map`: the user writes a key that means
something in their domain, and reconciliation becomes correct without them
knowing the marker exists.

`annotate_hydration_tree` skips any node that already carries the attribute, so
a `<For>` key survives hydration annotation rather than being overwritten.

### Diagnostics

- `<For>` without `key` is a compile error naming the attribute and saying why
  it is required. Defaulting to positional keys would silently reintroduce the
  behaviour `<For>` exists to prevent.
- `<For>` without `each` or `view`, and `<Show>` without `when`, are compile
  errors.
- An unknown capitalised tag keeps ADR 0006's existing message, now also listing
  the built-ins so `<Fore>` or `<show>` is a short trip.

## Consequences

**The closed set is a real constraint.** Adding `<Suspense>` later means editing
the macro, not just writing a component. That is the price of not opening
general component composition, and it is deliberate: the set stays small and
every member is a framework concept with runtime support behind it.

**ADR 0006's rule still holds for everything else.** `<MyComponent/>` is still an
error with the same message. A reader who knows only 0006 is not surprised by
`<Show>`, because the error text now lists the built-ins.

**`<For>` makes keyed reconciliation reachable.** Before this, the reconciler
worked but only for users who knew to stamp `data-krab-node-id` themselves —
which is to say, essentially nobody.

**Fallbacks are eager.** `fallback={…}` is a closure, so it is only *called* when
the branch is taken, but the closure itself is constructed on every render. That
is cheap and predictable; lazy construction would need a thunk type for no real
gain.

## Alternatives considered

**Lowercase `<show>` / `<for>`.** Avoids the ADR 0006 collision entirely. Rejected:
they would read as HTML elements, `for` is already an HTML attribute name, and
every neighbouring framework spells these capitalised — the unfamiliarity buys
nothing.

**Function-call helpers only: `{show(when, view, fallback)}`.** Works today with
no macro change, and remains available. Rejected as the *primary* form because
it does not solve the key-stamping problem — the user still has to know about
`data-krab-node-id` — and nesting reads poorly against surrounding markup.

**General component composition, as ADR 0006 deferred.** That is a much larger
design: a tag-to-path resolution rule, a props convention, children-as-props, and
a diagnostic story for each. Control flow does not need any of it, and shipping
control flow does not commit us to it either way.

## References

- Refines: [ADR 0006](0006-view-component-composition.md)
- Reconciler this depends on: `krab_client::patch_children`
- Plan: `internal/plans/reactive_core.md` Phase 4
