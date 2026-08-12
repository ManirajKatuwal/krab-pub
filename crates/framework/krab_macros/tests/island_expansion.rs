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

// ── Props that are deliberately not `Clone` ─────────────────────────────────
//
// The server half used to render as `inner(props.clone())`, which put a `Clone`
// bound on every island's props type even though `to_string` above it only
// borrowed. This island fails to compile if that clone ever comes back.

#[derive(Serialize, Deserialize)]
struct NoCloneProps {
    label: String,
}

#[island]
fn NoCloneIsland(props: NoCloneProps) -> krab_core::Node {
    Node::Text(props.label)
}

#[test]
fn island_props_do_not_have_to_be_clone() {
    let html = NoCloneIsland(NoCloneProps {
        label: "moved".to_string(),
    })
    .render();

    assert!(html.contains("data-island=\"NoCloneIsland\""));
    assert!(html.contains("data-krab-boundary-state=\"ssr\""));
    assert!(html.contains("moved"));
}

// ── Props that cannot be serialized ─────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct UnserializableProps {
    // `serde_json` refuses a map whose keys are not strings, so this fails to
    // encode as soon as it holds an entry.
    lookup: std::collections::HashMap<(i32, i32), String>,
}

#[island]
fn Unserializable(props: UnserializableProps) -> krab_core::Node {
    Node::Text(format!("{} entries", props.lookup.len()))
}

#[test]
fn a_props_encode_failure_is_reported_as_its_own_boundary_state() {
    let mut lookup = std::collections::HashMap::new();
    lookup.insert((1, 2), "value".to_string());

    let html = Unserializable(UnserializableProps { lookup }).render();

    // Not `state="ssr"` with an empty `data-props`, which is what
    // `unwrap_or_default()` produced: that surfaced in the browser as a client
    // decode error for a failure that happened on the server.
    assert!(
        html.contains("data-krab-boundary-state=\"props-encode-error\""),
        "expected the encode failure to be marked, got: {html}"
    );
    assert!(html.contains("data-props=\"\""));
    // The SSR markup is still rendered; only hydration is lost.
    assert!(html.contains("1 entries"));
}

#[test]
fn a_props_encode_failure_keeps_the_serde_message_out_of_the_markup() {
    let mut lookup = std::collections::HashMap::new();
    lookup.insert((3, 4), "value".to_string());

    let html = Unserializable(UnserializableProps { lookup }).render();

    // The message is derived from application data and this string is served to
    // every visitor.
    assert!(
        !html.contains("key must be a string"),
        "serde detail leaked into SSR output: {html}"
    );
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
