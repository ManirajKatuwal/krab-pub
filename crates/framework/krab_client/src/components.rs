//! Bundled demo islands — **deprecated, removed in 0.6.0.**
//!
//! `Counter`, `Toggle` and `Likes` exist to make the getting-started page and
//! `service_frontend` interactive out of the box. They are not framework
//! surface, and shipping them from a library crate is actively harmful:
//!
//! Every `#[island]` emits an `inventory::submit!` of an [`IslandDefinition`]
//! whose key is the *plain function name*. `inventory` links every submission
//! in the final binary into one registry, so these three names are claimed in
//! **every** consumer's bundle. A user who defines their own `Counter` island
//! ends up with two registry entries under the name `Counter`, and
//! [`hydrate`](crate::hydrate) resolves them with `find()` — first match wins,
//! decided by link order. The wrong component hydrates, and nothing reports it.
//!
//! [`IslandDefinition`]: crate::IslandDefinition
//!
//! # Migration
//!
//! Define the island in your own crate — the macro is the entire mechanism,
//! there is nothing to inherit from here:
//!
//! ```ignore
//! #[derive(serde::Serialize, serde::Deserialize, Clone)]
//! pub struct CounterProps { pub initial: i32 }
//!
//! #[island]
//! pub fn Counter(props: CounterProps) -> Node { /* ... */ }
//! ```
//!
//! Until 0.6.0 this module is compiled by the `demo-islands` feature, which is
//! on by default. Opt out with `default-features = false` (adding
//! `features = ["web"]` back if you want the hydration runtime), and the names
//! are yours.
//!
//! Note on the attribute: the `#[deprecated]` markers below sit on the props
//! types rather than on `Counter`/`Toggle`/`Likes` themselves. `#[island]`
//! moves the annotated function's attributes onto the hidden `*_impl` and emits
//! the public wrapper fresh, so an attribute written on the island function
//! never reaches the item a caller names. Props are named at every call site,
//! so deprecating them warns in the same place.
#![allow(non_snake_case)]
// The islands below are themselves the deprecated items; `#[island]` also names
// the props types in the code it generates. Warning here would only shout at
// the definitions, not at the callers the deprecation is aimed at.
#![allow(deprecated)]

// Gated per item rather than with a file-level `#![cfg(feature =
// "demo-islands")]`: that form removes the *module*, not just its contents, and
// `lib.rs` declares `pub mod components;` and re-exports `components::*`. With
// the feature off the crate then fails to compile with
// `unresolved import components`. Per-item leaves an empty module behind, which
// is exactly what the re-export needs.
#[cfg(feature = "demo-islands")]
use krab_core::signal::*;
#[cfg(feature = "demo-islands")]
use krab_core::{IntoNode, Node};
#[cfg(feature = "demo-islands")]
use krab_macros::{island, view};
#[cfg(feature = "demo-islands")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "demo-islands")]
#[derive(Serialize, Deserialize, Clone)]
#[deprecated(
    since = "0.4.0",
    note = "demo island, removed in 0.6.0: `Counter` is `inventory::submit`ed into every consumer's binary, so defining your own `Counter` island puts two entries under one name in the registry and `hydrate()`'s `find()` resolves them by link order. Define the island in your own crate with `#[island]`, or disable the `demo-islands` feature."
)]
pub struct CounterProps {
    pub initial: i32,
}

#[cfg(feature = "demo-islands")]
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

#[cfg(feature = "demo-islands")]
#[derive(Serialize, Deserialize, Clone)]
#[deprecated(
    since = "0.4.0",
    note = "demo island, removed in 0.6.0: `Toggle` is `inventory::submit`ed into every consumer's binary, so defining your own `Toggle` island puts two entries under one name in the registry and `hydrate()`'s `find()` resolves them by link order. Define the island in your own crate with `#[island]`, or disable the `demo-islands` feature."
)]
pub struct ToggleProps {
    pub initial: bool,
}

#[cfg(feature = "demo-islands")]
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

#[cfg(feature = "demo-islands")]
#[derive(Serialize, Deserialize, Clone)]
#[deprecated(
    since = "0.4.0",
    note = "demo island, removed in 0.6.0: `Likes` is `inventory::submit`ed into every consumer's binary, so defining your own `Likes` island puts two entries under one name in the registry and `hydrate()`'s `find()` resolves them by link order. Define the island in your own crate with `#[island]`, or disable the `demo-islands` feature."
)]
pub struct LikesProps {
    pub initial: i32,
}

#[cfg(feature = "demo-islands")]
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
