// Should fail: '-' twice in a row leaves an empty name segment.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <div data--testid="x"></div> }.render();
}
