#![allow(non_snake_case, unexpected_cfgs)]

use krab_core::{Element, Node, Render};
use krab_macros::island;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
struct GreetingProps {
    label: String,
}

#[island]
fn Greeting(props: GreetingProps) -> krab_core::Node {
    Node::Element(Element {
        tag: "span".to_string(),
        attributes: vec![],
        children: vec![Node::Text(props.label)],
        events: vec![],
    })
}

#[test]
fn island_server_wrapper_emits_boundary_metadata() {
    let html = Greeting(GreetingProps {
        label: "Hello".to_string(),
    })
    .render();

    assert!(html.contains("data-island=\"Greeting\""));
    assert!(html.contains("data-krab-boundary=\"Greeting\""));
    assert!(html.contains("data-krab-boundary-state=\"ssr\""));
    assert!(html.contains("data-krab-boundary-id=\"Greeting:"));
    assert!(html.contains("data-krab-node-id=\"Greeting:"));
    assert!(html.contains(">Hello</span>"));
}
