// Should fail: a misspelt attribute on `<Suspense>` must not be ignored.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <Suspense fallback={|| view! { <p>"loading"</p> }} fallbak={1}>
            <p>"content"</p>
        </Suspense>
    }
    .render();
}
