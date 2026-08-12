// Should fail: a fragment that is never closed.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <>"hello" }.render();
}
