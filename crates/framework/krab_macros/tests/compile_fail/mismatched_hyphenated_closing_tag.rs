// Should fail: the diagnostic must print the full hyphenated names, not just
// the first segment of each.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <my-widget>"x"</my-panel> }.render();
}
