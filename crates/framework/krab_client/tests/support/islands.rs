//! Test-local islands for the browser suites.
//!
//! These are the `Counter` and `Toggle` islands `krab_client` shipped behind
//! its `demo-islands` feature until 0.6.0, moved here verbatim when the feature
//! was removed. The suites' hand-written SSR markup mirrors exactly what these
//! render, so the component bodies must stay as they are.
//!
//! Include with `#[path = "support/islands.rs"] mod islands;`. `#[island]`
//! registers each one through `inventory`, so including the module is enough
//! for `hydrate()` to find them; nothing needs to name them.

#![allow(dead_code, non_snake_case)]

use krab_core::signal::*;
use krab_core::{IntoNode, Node};
use krab_macros::{island, view};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct CounterProps {
    pub initial: i32,
}

#[island]
pub fn Counter(props: CounterProps) -> Node {
    let (count, _set_count) = create_signal(props.initial);
    view! {
        <button on:click={ move |_| _set_count.update(|c| *c += 1) }>
            "Count: " <span>{ move || count.get().into_node() }</span>
        </button>
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ToggleProps {
    pub initial: bool,
}

#[island]
pub fn Toggle(props: ToggleProps) -> Node {
    let (on, _set_on) = create_signal(props.initial);
    view! {
        <div>
            <button on:click={ move |_| _set_on.update(|v| *v = !*v) }>
                "Toggle"
            </button>
            <span>{ move || if on.get() { " ON" } else { " OFF" }.into_node() }</span>
        </div>
    }
}
