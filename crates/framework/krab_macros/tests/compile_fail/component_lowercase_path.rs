// Should fail: a tag containing `::` is a component path, and component names
// are capitalised. `ui::card` is neither a component nor an HTML element.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <ui::card/> }.render();
}
