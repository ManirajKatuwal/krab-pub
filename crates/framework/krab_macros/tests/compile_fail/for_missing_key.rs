// Should fail: `<For>` requires `key`. Falling back to positional matching
// would silently reintroduce the behaviour `<For>` exists to prevent —
// inserting a row renumbers every row after it, and they lose focus and DOM
// state. See ADR 0008.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <For
            each={|| vec![1u32, 2]}
            view={|item: u32| view! { <li>{item.to_string()}</li> }}
        />
    }
    .render();
}
