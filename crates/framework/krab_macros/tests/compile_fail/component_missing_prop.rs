// Should fail: `count` is neither written out nor filled by `..`, so the props
// struct literal is incomplete. rustc names the missing field; the error points
// at the component tag.
#![allow(non_snake_case)]
use krab_core::{Node, Render};
use krab_macros::view;

struct CardProps {
    title: String,
    count: i32,
}

fn Card(props: CardProps) -> Node {
    view! { <h2>{props.title}{props.count}</h2> }
}

fn main() {
    let _ = view! { <Card title="Inbox"/> }.render();
}
