// Should fail: a name segment must follow the '-' separator.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <div data-="x"></div> }.render();
}
