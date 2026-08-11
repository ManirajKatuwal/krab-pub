// Should fail: a typo'd attribute on a control-flow tag must not be silently
// ignored — `fallbck` would look like a working fallback that never renders.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <Show when={|| false} fallbck={|| view! { <p>"no"</p> }}>
            <p>"yes"</p>
        </Show>
    }
    .render();
}
