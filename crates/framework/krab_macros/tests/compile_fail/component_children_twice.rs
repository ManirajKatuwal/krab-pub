// Should fail: `children` given both as an attribute and as content.
#![allow(non_snake_case)]
use krab_core::{Node, Render};
use krab_macros::view;

struct PanelProps {
    children: Node,
}

fn Panel(props: PanelProps) -> Node {
    props.children
}

fn main() {
    let _ = view! { <Panel children={Node::Text("a".into())}>"b"</Panel> }.render();
}
