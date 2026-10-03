// Should fail: `..` fills omitted component props from `Default`, and an HTML
// element has no props.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <div class="a" ../> }.render();
}
