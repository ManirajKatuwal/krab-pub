// Should fail: `titel` is not a field of `CardProps`. The error is rustc's own
// struct-literal error, and it must point at the misspelt attribute rather than
// at the macro invocation.
#![allow(non_snake_case)]
use krab_core::{Node, Render};
use krab_macros::view;

struct CardProps {
    title: String,
}

fn Card(props: CardProps) -> Node {
    view! { <h2>{props.title}</h2> }
}

fn main() {
    let _ = view! { <Card titel="Inbox"/> }.render();
}
