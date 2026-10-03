// Should fail: `<Suspense>` without `fallback` would render an empty hole
// while it loads.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <Suspense>
            <p>"content"</p>
        </Suspense>
    }
    .render();
}
