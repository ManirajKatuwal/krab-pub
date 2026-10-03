# ADR 0013: Component Tags in `view!`

## Status

**Accepted** — 2026-09-30. Implemented in the same change, for release in
`0.6.0`.

Supersedes [ADR 0006](0006-view-component-composition.md). Keeps
[ADR 0008](0008-view-control-flow-tags.md): `<Show>` and `<For>` retain their
built-in meaning.

## Context

ADR 0006 made a capitalised tag in `view!` a compile error. The alternative at
the time was emitting `<MyComponent>` as literal markup that no browser renders,
and the error stopped that. It was explicit that the decision reserved the
syntax rather than ruling it out, and it listed what a design would have to
settle before the syntax could be spent:

1. a rule mapping tag names to paths — is `<widgets::Card/>` legal?
2. a props-construction convention, agreeing with `#[island]`'s;
3. children, as props;
4. whether a capitalised tag resolves to a function or a type;
5. a diagnostic for each way to get it wrong.

The cost of not having composition has been paid at every call site since:
`{card(CardProps { title: "Inbox".to_string(), children: view! { ... } })}` is
the only spelling, and nesting two components reads inside-out. Every
JSX-family framework a Krab user is likely to know — React, Solid, Leptos,
Dioxus, Yew — spells this `<Card title="Inbox">...</Card>`, and 0006's error
message was the most common first contact with `view!`'s limits.

The framework already had the convention the design needs. `#[island]` requires
exactly one argument, a props struct, and a `krab_core::Node` return:
`fn Counter(props: CounterProps) -> Node`. A composition syntax that expands to
that same call makes every island usable as a tag with no further work, and
makes "component" and "island" differ in exactly one respect — whether the
browser hydrates it — which is the distinction the framework wants users to
think in.

## Decision

**A tag whose last path segment starts with an uppercase ASCII letter is a
component call.** Lowercase tags remain HTML elements, exactly as before. The
answers to 0006's five questions:

### 1. Tag name → path

A component tag is a Rust path, written as it would be in an expression:
`<Card/>`, `<ui::Card/>`, `<crate::ui::Card/>`. The path is emitted verbatim, so
it resolves by ordinary Rust name resolution at the call site and `use`
statements work as they always do.

- A tag containing `::` is always a path. Its last segment must be capitalised;
  `<ui::card/>` is a compile error, not a guess.
- A capitalised tag may not contain `-` or `:`. Custom elements are lowercase
  by the HTML specification, so `<My-Widget>` is an error rather than markup.
- `Show` and `For` are **reserved** as single-segment names (ADR 0008). A user
  component named `Show` is reachable through a path: `<ui::Show/>`.
- The closing tag must repeat the opening path exactly. `</Card>` does not close
  `<ui::Card>`, even when both resolve to one function: a macro sees spelling,
  not resolution, and "the same spelling" is the only rule it can check.

The capitalisation rule cannot misread valid markup for the reason 0006 gave:
no HTML or SVG element name starts with an uppercase letter or contains `::`.

### 2. Props convention

```rust
view! { <ui::Card title="Inbox" count={n}/> }
// expands to
ui::Card(ui::CardProps { title: ::core::convert::Into::into("Inbox"), count: n })
```

(Precisely: the props value is built first, then the call runs inside a
context scope — `with_owner(move || ui::Card(props))`; see ADR 0014.)

- The component is a **function**, called with one argument of type
  `{Name}Props` found at the same path. This is `#[island]`'s signature, so an
  island is a component. (Question 4: a function, not a type. A type would need
  a trait for "render", and a second convention beside `#[island]`'s.)
- The props type name is a convention, not a lookup. A proc macro cannot see the
  function's signature, so it must be told the type's name; `{Name}Props` is the
  name the framework's own examples, templates, and tests already use.
- Each attribute names a struct field. Hyphens become underscores
  (`aria-label` → `aria_label`); a Rust keyword becomes a raw identifier
  (`type` → `r#type`).
- **A string literal is passed through `Into::into`; a braced expression is
  passed through untouched.** The literal case needs a conversion — `"x"` is a
  `&'static str` and the field is usually a `String` — and `Into` lets the same
  literal serve `String`, `&'static str`, and `Cow<'static, str>` fields.
  Expressions are not converted, because a blanket `.into()` breaks three
  things that work in a plain struct literal: unsized coercion
  (`Box::new(move || ...)` into a `Box<dyn Fn()>` field — there is no
  `From` impl for that), integer-literal inference (`Into::into(3)` into a
  `u64` field falls back to `i32` and fails with "the trait bound
  `u64: From<i32>` is not satisfied"), and error quality (a type mismatch reported as a
  missing `From` impl rather than "expected `String`, found `&str`" at the
  expression). The rule the author has to hold is short: literals convert,
  expressions are exact.
- Every field is set explicitly unless the tag contains a bare **`..`**, which
  expands to Rust's struct-update syntax, `..Default::default()`, and so requires
  the props type to implement `Default`. It is opt-in, and it is spelled the way
  Rust spells the same thing. The alternative — filling omitted props from
  `Default` automatically — was rejected: it would make `Default` a requirement
  of every props type (islands included), and a forgotten required prop would
  silently render with its default instead of failing with rustc's
  "missing field `count` in initializer of `CardProps`". `krab_core::Node` now
  implements `Default` (an empty fragment) so that a props type holding
  `children` can still derive it.

### 3. Children

Content between a component's tags is passed as the `children` field, typed
`krab_core::Node`. One child is passed as itself; several are wrapped in a
`Node::Fragment`, so the field is always one node. A self-closing component
sets no `children` field at all, which leaves it to `..` or to a props type that
has no such field. Writing `children={...}` as an attribute *and* giving content
is a compile error.

Children are an ordinary field, not a separate argument, because the island
signature has exactly one argument. The consequence is that an island cannot
take children — `Node` is not `Serialize`, and an island's props must be — and
that consequence is correct: the browser re-runs an island from its serialized
props alone, so there is nowhere for server-built children to come from.

### 5. Diagnostics

| Mistake | Reported by | Points at |
|---|---|---|
| unknown prop (`titel=`) | rustc: "struct `CardProps` has no field named `titel`" | the attribute — every field identifier is spanned on its attribute |
| missing prop | rustc: "missing field `count` in initializer of `CardProps`" | the tag name |
| unknown component | rustc: "cannot find function `Card`" | the tag name |
| `on:click` on a component | `view!`: a component has no DOM node to attach to; pass the handler as a prop | the directive |
| `xlink:href` or another namespaced name on a component | `view!`: props are fields, `:` has no field spelling | the attribute |
| `<ui::card/>` | `view!`: a path tag names a component, and components are capitalised | the last segment |
| `</Card>` closing `<ui::Card>` | `view!`: mismatched closing tag, printing both paths | the closing tag |
| `..` on an element or `<Show>`/`<For>` | `view!`: only components take props | the `..` |

Errors that rustc already reports well are left to rustc. Re-implementing a
field check in the macro would be impossible anyway — the macro cannot see the
struct — and spans are what make rustc's message land on the right token.

## Consequences

**`<Card title="x">...</Card>` works**, including for every existing
`#[island]`. Interpolating a call, `{card(props)}`, keeps working, so no
existing call site changes. Nothing can have depended on the old behaviour
either: since 0006, every capitalised tag other than `Show`/`For` failed to
compile.

**Component functions are capitalised**, which trips rustc's `non_snake_case`
lint on the function, exactly as it already did for every `#[island]`. A
component needs `#[allow(non_snake_case)]` on the function or its module. This
is the same trade Leptos and Dioxus make, and the alternative —
mapping `<Card>` to `fn card` — would make an island's tag name differ from its
function name.

**`{Name}Props` is now a name the framework relies on.** Renaming a props type
away from that convention makes its component unusable as a tag (the function
still works when called directly). The error is rustc's "cannot find struct
`CardProps`", pointing at the tag. For the same reason an unqualified tag needs
both names in scope — `use ui::{Card, CardProps};` — or is written as a path
tag, `<ui::Card/>`, which needs neither.

**Reactivity is unchanged.** A component is called once, while the enclosing
`view!` builds its tree. Props are plain values; a prop that should update in
place is a signal or a closure, as it would be when calling the function by
hand. Component tags add syntax, not a lifecycle. The one thing a tag adds
over a direct call is a context scope: the call runs inside
`krab_core::signal::with_owner`, so contexts a component provides reach what
it renders and not its siblings. See
[ADR 0014](0014-context-api-and-owners.md), written alongside this one.

**`Show` and `For` stay reserved.** Adding a third built-in tag later would be a
breaking change for any user component of that name used unqualified, so the
set should grow only with a deprecation cycle.

## Alternatives considered

**Keep ADR 0006.** Rejected: its rationale was that the design was unsettled,
not that composition was wrong, and each of its open questions has an answer
above that agrees with the existing island convention.

**Builder-pattern props** (`Card::props().title("x").build()`, as Leptos
generates). Rejected: it requires a derive or attribute macro on every
component to produce the builder, and a second, parallel convention beside
`#[island]`'s plain struct. A struct literal is something every Rust user
already reads.

**`.into()` on every attribute value.** Rejected for the three reasons in
§2 — coercion, inference, and diagnostics. The literal-only rule keeps the one
case that needs a conversion and loses nothing else.

**Always fill omitted props from `Default`.** Rejected in §2: it trades a
compile error for a silently defaulted prop.

**Resolve the props type from the function signature** instead of by naming
convention. Not possible in a proc macro, which runs before type checking. A
trait-based indirection (`<Card as Component>::Props`) would work, but it needs
an impl per component — another macro — for a name the convention already
gives.

## References

- Parser and codegen: `crates/framework/krab_macros/src/view.rs`
  (`parse_tag_name`, `prop_field`, `Element::into_component`,
  `impl ToTokens for Component`)
- Runtime tests: `crates/framework/krab_macros/tests/view_expansion.rs`
  (component section), `tests/island_expansion.rs`
  (`island_used_as_a_view_tag_renders_its_ssr_wrapper`)
- Compile-fail cases: `crates/framework/krab_macros/tests/compile_fail/component_*.rs`,
  `rest_on_element.rs`
- Island convention: [`docs/architecture/hydration.md`](../architecture/hydration.md)
- User guide: [`docs/guides/getting_started.md`](../guides/getting_started.md),
  "Composing components"
