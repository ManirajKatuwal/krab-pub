#![allow(non_snake_case)]

use krab_core::signal::*;
use krab_core::{IntoNode, Node};
use krab_macros::{island, view};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct CounterProps {
    pub initial: i32,
}

#[island]
#[allow(non_snake_case)]
pub fn Counter(props: CounterProps) -> Node {
    let (count, _set_count) = create_signal(props.initial);
    // The dynamic count is wrapped in a <span> so the hydration algorithm can
    // find a real DOM element to anchor the reactive update against.
    // Without this, adjacent text nodes are merged by the browser and the
    // Dynamic node has no corresponding DOM node to replace on signal change.
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
#[allow(non_snake_case)]
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

#[derive(Serialize, Deserialize, Clone)]
pub struct LikesProps {
    pub initial: i32,
}

#[island]
#[allow(non_snake_case)]
pub fn Likes(props: LikesProps) -> Node {
    let (likes, _set_likes) = create_signal(props.initial);
    view! {
        <div>
            <button on:click={ move |_| _set_likes.update(|v| *v += 1) }>
                "Like"
            </button>
            <span>{ move || likes.get().into_node() }</span>
        </div>
    }
}
