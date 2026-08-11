// Should fail: `<For>` renders each row through `view`, so children are
// ambiguous — they would be silently dropped.
use krab_core::Render;
use krab_macros::view;

fn main() {
    let _ = view! {
        <For
            each={|| vec![1u32]}
            key={|item: &u32| *item}
            view={|item: u32| view! { <li>{item.to_string()}</li> }}
        >
            <li>"ignored"</li>
        </For>
    }
    .render();
}
