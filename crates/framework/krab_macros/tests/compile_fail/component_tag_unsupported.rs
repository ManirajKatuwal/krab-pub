// Should fail: `view!` has no component composition. Previously this silently
// emitted the literal markup `<MyComponent>`, which no browser renders.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! { <MyComponent/> }.render();
}
