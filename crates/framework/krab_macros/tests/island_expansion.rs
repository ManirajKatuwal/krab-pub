// `unexpected_cfgs` / `unused_variables`: the `view!` macro gates event
// listeners behind the *consuming* crate's `web` feature, which this test crate
// does not declare. Handler closures are therefore compiled out here, leaving
// their captures unused.
#![allow(non_snake_case, unexpected_cfgs, unused_variables)]

use krab_core::signal::create_signal;
use krab_core::{Element, IntoNode, Node, Render};
use krab_macros::{island, view};
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

// ── README / docs/architecture/design.md island example ─────────────────────
//
// This is the counter example published in `README.md` and
// `docs/architecture/design.md`. It is duplicated here verbatim so the compiler
// rejects the docs drifting away from the real `#[island]` contract: exactly
// one serialisable props argument, returning `krab_core::Node`.

#[derive(Clone, Serialize, Deserialize)]
pub struct CounterProps {
    pub initial: i32,
}

#[island]
pub fn Counter(props: CounterProps) -> krab_core::Node {
    let (count, set_count) = create_signal(props.initial);
    view! {
        <button on:click={move |_| set_count.update(|n| *n += 1)}>
            "Count: "
            {move || count.get().into_node()}
        </button>
    }
}

#[test]
fn readme_counter_example_renders_on_the_server() {
    let html = Counter(CounterProps { initial: 7 }).render();

    assert!(html.contains("data-island=\"Counter\""));
    assert!(html.contains("<button"));
    assert!(html.contains("Count: "));
    assert!(html.contains("7"));
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
