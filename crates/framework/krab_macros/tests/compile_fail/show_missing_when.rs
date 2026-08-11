// Should fail: `<Show>` without `when` has no condition to branch on.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <Show fallback={|| view! { <p>"no"</p> }}>
            <p>"yes"</p>
        </Show>
    }
    .render();
}
