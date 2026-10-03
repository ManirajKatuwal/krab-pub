# ADR 0015: Reactive Attributes in `view!`

## Status

**Accepted** — 2026-09-30. Implemented in the same change, for release in
`0.6.0`.

Builds on [ADR 0001](0001-hydration-markers.md) (hydration adopts server
markup) and [ADR 0008](0008-view-control-flow-tags.md) (reactivity through
`Node::Dynamic` and effects).

## Context

`view!` stringified every attribute value once, when the tree was built:
`krab_core::Attribute { name, value: (#value).to_string() }`. Text children
could be reactive (a `move ||` closure becomes `Node::Dynamic`), attributes
could not — `disabled={move || busy.get()}` failed to compile because a closure
has no `Display`. The getting-started guide and the server-functions reference
both carried a callout telling users to re-render the whole element inside a
dynamic block instead, which rebuilds its subtree (losing focus and input
state) to flip one attribute.

## Decision

**An attribute whose value is a closure literal is dynamic.** `view!` expands
it to `krab_core::Attribute::dynamic(name, closure)`; every other value keeps
the old expansion, `Attribute::new(name, (value).to_string())`.

### Representation

`Attribute` gains one field rather than changing `value`'s type:

```rust
pub type DynamicAttributeValue = Rc<dyn Fn() -> Option<String>>;

pub struct Attribute {
    pub name: String,
    pub value: String,                          // unused (empty) when dynamic
    pub dynamic: Option<DynamicAttributeValue>, // None = static
}

impl Attribute {
    pub fn new(name: String, value: String) -> Self;
    pub fn dynamic<F, V>(name: impl Into<String>, source: F) -> Self
    where F: Fn() -> V + 'static, V: IntoAttributeValue;
    pub fn is_dynamic(&self) -> bool;
    pub fn current_value(&self) -> Option<String>;
}

pub trait IntoAttributeValue { fn into_attribute_value(self) -> Option<String>; }
```

`None` from the source means *absent*. `IntoAttributeValue` is implemented for
strings, `Cow<str>`, `char`, the integer and float types, `Option<T>` (absent on
`None`) and `bool`, which is the boolean-attribute form: `true` renders the
attribute present with an empty value, `false` omits it. HTML reads a boolean
attribute's presence, not its value — `disabled="false"` is disabled — so
stringifying a `bool` would have been wrong.

Keeping `value: String` means every reader of a static attribute (hydration
markers, `data-island`, user code matching on attributes) is unchanged. The
`Rc` keeps `Node` `!Send`, which it already was.

The detection is syntactic (`syn::Expr::Closure`): a macro cannot see types. A
closure held in a variable is still stringified and still fails to compile; the
fix is `{move || f()}`. This matches how `on:` handlers and `Node::Dynamic`
interpolation are already recognised by shape.

### Server

`Element::render` calls `current_value()`: a dynamic attribute renders its
value as of render time and is omitted on `None`. Evaluation is lazy — building
the tree does not call the source — so an element built inside an effect does
not subscribe that effect to the attribute's signals.

### Browser

`krab_client` binds one `create_effect_scoped` effect per dynamic attribute to
the element, at hydration (`hydrate_element`), at creation
(`create_element_node`) and when the reconciler reuses an element for a new
render (`patch_dom`, which re-binds so the sources capture the new render's
state — the same reasoning that already applied to listeners). Handles are
stored in a registry keyed by an expando on the element and released by
`release_dom_node_resources`, so removal, replacement and `unmount` dispose
them like event closures.

Each run compares before writing: `set_attribute` when the value differs,
`remove_attribute` on `None`.

**Hydration agreement.** SSR and the first client run evaluate the same source
over the same signal state, so hydration normally finds the attribute already
correct and writes nothing — the hydration walk binds to the element; it does
not rewrite it. If the two disagree (client state differs), the attribute is
patched; this is not counted as a boundary mismatch, as it is a state
difference rather than a structural one.

**Properties vs attributes.** For three attributes the attribute is only the
*default* once a form control exists: `value` on `input`/`textarea`/`select`,
`checked` on `input`, `selected` on `option`. After the user edits the
control, changing the attribute no longer changes what it shows. For these the
effect also writes the live DOM property (`value` as a string, `checked` /
`selected` as `value.is_some()`), comparing first so an unchanged `value` does
not move the caret. The hydration run is the exception: it never writes the
property, because the property may already hold input the user made before the
bundle loaded, and the attribute already matches the server. Every other
attribute — including boolean ones like `disabled` and `hidden` — reflects, so
setting or removing the attribute is sufficient.

## Consequences

- **Breaking for struct literals.** `krab_core::Attribute { name, value }` no
  longer compiles; add `dynamic: None` or use `Attribute::new`. The workspace's
  own literals (the `#[island]` wrapper, tests) now use `Attribute::new`.
  `Attribute`'s `Debug` output prints `"<dynamic>"` for a dynamic value instead
  of evaluating it (formatting must not read signals).
- Reactive state no longer needs an element re-rendered through `Node::Dynamic`
  to reach an attribute; the element and its subtree keep their DOM identity.
- A closure-valued attribute that previously failed to compile now compiles;
  no previously compiling program changes meaning, since a closure literal was
  never a valid `Display` value.
- Component props are unaffected: an attribute on a component tag is a struct
  field and is passed through as written (ADR 0013).

## Alternatives considered

**Change `value` to an enum (`Static(String)` / `Dynamic(..)`).** Rejected: it
breaks every reader of `.value`, not just constructors, for no gain over an
extra field.

**Re-render the element through `Node::Dynamic`.** The status quo workaround;
it rebuilds the subtree on every change and loses focus and selection.

**Always write attributes and properties.** Rejected for hydration: writing the
`value` property during adoption would erase text typed before the bundle
loaded, and writing an unchanged attribute produces mutation records for
nothing.

## References

- `crates/framework/krab_core/src/lib.rs` — `Attribute`, `IntoAttributeValue`,
  `Element::render`
- `crates/framework/krab_macros/src/view.rs` — `impl ToTokens for Attribute`
- `crates/framework/krab_client/src/resources.rs` — `bind_dynamic_attributes`,
  `apply_dynamic_attribute`
- Tests: `krab_core` `render_safety_tests::dynamic_*`, `krab_macros`
  `tests/view_expansion.rs` (reactive attributes), `krab_client`
  `tests/attribute_browser.rs`
