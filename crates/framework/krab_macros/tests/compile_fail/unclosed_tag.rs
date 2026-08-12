// Should fail: an element that is never closed.
//
// The interesting part is *which* error: before `parse_children` existed, the
// exhausted stream reached `Node::parse` and this reported "view! macro body is
// empty" on a body that plainly is not.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <div>"hello" }.render();
}
