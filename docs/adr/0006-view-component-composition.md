# ADR 0006: `view!` Has No Component Composition

## Status

Accepted

## Context

`view!` parses a tag name and emits it verbatim as an HTML element:

```rust
// crates/framework/krab_macros/src/lib.rs, ToTokens for Node::Element
krab_core::Node::Element(krab_core::Element {
    tag: #name.to_string(),
    ...
})
```

So `view! { <MyComponent/> }` produced the literal markup `<MyComponent>`. No
browser renders that element, nothing in the workspace tested for it, and the
macro reported no error. A user coming from JSX, Leptos, or Dioxus — all of
which spell component invocation exactly this way — got silently broken HTML.

Two things were tangled together here and are worth separating:

1. **Hyphenated and namespaced names.** Tag and attribute names parsed as
   `syn::Ident`, so `data-*`, `aria-*`, `xlink:*`, custom elements, and even
   `type` and `for` (Rust keywords) were unrepresentable. This was a defect with
   no design question attached, and it is fixed — see the grammar in
   `parse_html_name`.
2. **Component composition.** Whether `<MyComponent/>` should invoke a function
   is a design decision, not a defect. It is what this ADR settles.

The framework already has a composition mechanism. Any function returning
`krab_core::Node` can be interpolated:

```rust
view! { <div>{sidebar(props)}</div> }
```

and `#[island]` marks the subset of components that additionally hydrate in the
browser. Neither needs new syntax.

Adding `<MyComponent/>` would require, at minimum: a rule mapping tag names to
paths (is `<my::Widget/>` legal? `<widgets::Card/>`?), a props-construction
convention including children-as-props, a decision on whether a capitalised tag
resolves to a function or a type, and a diagnostic story for each. That is a
language surface, and designing it under the current remediation is scope the
work does not need.

The immediate cost of *not* deciding is the silent failure. That is what has to
stop, regardless of which way the design goes later.

## Decision

**`view!` does not support component composition, and a capitalised tag name is
a compile error.**

No HTML or SVG element name begins with an uppercase ASCII letter — SVG's
camelCase names (`linearGradient`, `clipPath`, `feGaussianBlur`) all start
lowercase — so the rule is unambiguous and cannot reject valid markup.

```
error: `view!` has no component composition, so <MyComponent> would be emitted
       as a literal HTML tag named 'MyComponent'.
         Call the function and interpolate its node instead:
           view! { <div>{my_component(props)}</div> }
         For an interactive component, annotate it with #[island].
```

Composition remains: call the function, interpolate the `Node`.

## Consequences

**Users coming from JSX-family frameworks hit a compile error rather than
broken output.** The error names the working alternative, so the cost is one
diagnostic read rather than a debugging session over markup that renders as
nothing.

**Interpolation is more verbose than `<MyComponent/>`** — `{my_component(props)}`
rather than attribute syntax, and props are constructed explicitly rather than
gathered from attributes. This is the accepted cost.

**The decision is reversible.** Rejecting capitalised tags today does not
constrain a future design; it reserves the syntax. If component composition is
later added, the error becomes an implementation, and no existing `view!` call
site changes — nothing can currently be relying on the old behaviour, because
the old behaviour produced markup that does not work.

**`view!` remains an HTML templating macro, not a component DSL.** That is the
honest description of what it is, and the documentation should say so rather
than implying a component model that does not exist.

## Alternatives considered

**Implement minimal component composition** — capitalised tag calls a function,
attributes become props-struct fields. Rejected for this change: the props
convention has to agree with `#[island]`'s, children-as-props needs a rule, and
path-qualified tags need a grammar. Doing it badly under time pressure would
create a syntax that has to be broken later, which is worse than not having it.

**Leave the silent behaviour and document it** — rejected. The failure mode is
markup that renders as nothing, with no error at any stage. Documentation does
not reach someone who did not know to look.

**Warn instead of error** — rejected. The generated output is never what the
author wanted, so there is no case in which compiling on is the right outcome.

## References

- Grammar and diagnostic: `crates/framework/krab_macros/src/lib.rs`
  (`parse_html_name`, `impl Parse for Element`)
- Compile-fail case: `crates/framework/krab_macros/tests/compile_fail/component_tag_unsupported.rs`
- Hydration and island model: [`docs/architecture/hydration.md`](../architecture/hydration.md)
- Remediation plan: `internal/plans/framework_viability.md` Phase 4 (internal
  planning document, not distributed)
