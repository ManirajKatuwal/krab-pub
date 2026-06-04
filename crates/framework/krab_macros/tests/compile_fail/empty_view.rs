// Should fail: empty view! macro body.
use krab_macros::view;
use krab_core::Render;

fn main() {
    let _ = view! { }.render();
}
