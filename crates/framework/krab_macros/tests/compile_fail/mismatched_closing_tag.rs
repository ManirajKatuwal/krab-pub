// Should fail: mismatched closing tag </span> for <div>.
use krab_macros::view;
use krab_core::Render;

fn main() {
    let _ = view! { <div>"hello"</span> }.render();
}
