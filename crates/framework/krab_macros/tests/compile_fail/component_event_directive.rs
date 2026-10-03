// Should fail: `on:click` attaches a DOM listener, and a component has no DOM
// node of its own. The diagnostic points at the directive and suggests passing
// the handler as a prop instead.
#![allow(non_snake_case)]
use krab_core::{Node, Render};
use krab_macros::view;

struct ButtonProps {
    label: String,
}

fn Button(props: ButtonProps) -> Node {
    view! { <button>{props.label}</button> }
}

fn main() {
    let _ = view! { <Button label="Go" on:click={|_| {}}/> }.render();
}
