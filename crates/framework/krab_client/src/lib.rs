//! Browser-side runtime for Krab's island architecture.
//!
//! Krab renders a page on the server and ships it as HTML. Only the
//! interactive parts — *islands* — are then brought to life in the browser by
//! this crate. It is the WASM half of the framework: it adopts the
//! server-rendered DOM instead of rebuilding it, wires event listeners onto the
//! nodes that are already there, and keeps [`Node::Dynamic`] regions in sync
//! with the reactive graph re-exported from `krab_core`.
//!
//! # The `web` feature
//!
//! Everything that touches the DOM is behind the `web` feature. Without it the
//! crate still compiles — so a workspace-wide `cargo check` on the host target
//! succeeds — but [`hydrate`] does nothing and the DOM entry points below are
//! not compiled at all. Anything built for the browser must enable `web`.
//!
//! The optional `debug` feature adds informational `console.log` tracing of the
//! hydration pass. Warnings and errors are always reported; `debug` only
//! controls the chatter, so production bundles stay quiet.
//!
//! # The hydration model
//!
//! The server emits each island as an element carrying `data-island` (the
//! registered component name), `data-props` (its JSON props), and a
//! `data-krab-boundary-id`. [`hydrate`] finds those elements, rebuilds each
//! island's virtual tree by calling the factory registered for its name, and
//! walks that tree against the live DOM:
//!
//! - a matching element is **reused**, and its listeners are attached to it;
//! - a text node whose content differs is **patched in place**, which is what
//!   lets a selection or an IME composition survive hydration;
//! - only a genuine shape mismatch causes a node to be replaced.
//!
//! Every boundary is stamped with the outcome — `data-krab-boundary-state`
//! (`ok`, `patched`, `error`, `missing-definition`, …) and
//! `data-krab-boundary-mismatches` — so monitoring can see a drifting boundary
//! without reading the console.
//!
//! That stamp is also what makes hydration **idempotent**. The server ships
//! `data-krab-boundary-state="ssr"`; a boundary whose state has moved off `ssr`
//! is skipped, so a second pass cannot bind a second copy of every handler.
//! [`unmount`] reverses a pass — releasing event closures and dynamic-region
//! effects, and clearing the stamp — so `unmount` followed by
//! [`hydrate_within`] is a supported re-hydration cycle.
//!
//! # Usage
//!
//! Islands register themselves; the application only has to call [`hydrate`]
//! once the WASM module is loaded.
//!
//! ```ignore
//! use krab_client::{create_signal, hydrate};
//! use krab_macros::{island, view};
//!
//! #[island]
//! fn Counter(start: i32) -> Node {
//!     let (count, set_count) = create_signal(start);
//!     view! {
//!         <button on:click={move |_| set_count.set(count.get() + 1)}>
//!             {move || count.get().to_string()}
//!         </button>
//!     }
//! }
//!
//! // Exported entry point, called by the page's module script once the WASM
//! // module is instantiated. NOT `#[wasm_bindgen(start)]`: `krab_client`
//! // already owns the bundle's single start function, and a second one is a
//! // wasm-bindgen error.
//! #[wasm_bindgen]
//! pub fn krab_boot() {
//!     hydrate();
//! }
//! ```
//!
//! The page loads it from a same-origin module file (an inline script is
//! blocked by Krab's CSP, `script-src 'self' 'wasm-unsafe-eval'`):
//!
//! ```js
//! import init, { krab_boot } from '/pkg/my_app.js';
//! init().then(() => krab_boot());
//! ```
//!
//! `examples/reference_apps/islands_rpc` does exactly this, and also starts the
//! client router after hydrating.
//!
//! To bring up a fragment that arrived after the initial load — a modal, a
//! client-routed view — hydrate just that subtree, and release it when it goes
//! away:
//!
//! ```ignore
//! krab_client::hydrate_within(&panel);
//! // …later…
//! krab_client::unmount(&panel);
//! panel.remove();
//! ```

extern crate self as krab_client;

use krab_core::Node;
use wasm_bindgen::prelude::*;
use web_sys::console;

// The runtime is split along the seams it already had inside one file:
//
// - `hydration` walks server-rendered islands and stamps each boundary;
// - `reconcile` builds and patches DOM for `Node::Dynamic` regions after
//   hydration, and is the only code that creates DOM from a vnode;
// - `resources` owns the two registries both of them write to — event-listener
//   closures and live dynamic regions — and releases them.
//
// Only `hydration` exists without `web`, because `hydrate()` is a `#[wasm_bindgen]`
// export that must link (as a no-op) in a build that leaves the DOM out.
mod hydration;
// Per-island panic isolation: the JS `try`/`catch` trampoline and the panic
// hook that records what trapped. Only wasm32 needs it — everywhere else
// unwinding is real and `catch_unwind` does the job.
#[cfg(all(feature = "web", target_arch = "wasm32"))]
mod isolation;
#[cfg(feature = "web")]
mod reconcile;
#[cfg(feature = "web")]
mod resources;

pub use hydration::hydrate;
#[cfg(feature = "web")]
pub use hydration::{
    hydrate_island, hydrate_within, hydrate_within_selector, log_hydration_diagnostic, unmount,
};
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub use reconcile::build_dom_for_test;
#[cfg(feature = "web")]
pub use resources::{attribute_effect_count, dynamic_region_count, event_closure_count};

pub use krab_core::signal::*;

pub mod router;

// Re-exported from `krab_core` so an island reaches it without importing a
// second crate. The type is transport-independent and lives there.
pub use krab_core::action::{create_action, Action};
pub use krab_core::resource::{
    create_resource, create_resource_with_initial, Resource, ResourceState,
};

/// Spawn a future on the browser's task queue.
///
/// Island event handlers are synchronous — `on:click` takes an `FnMut(Event)` —
/// but `#[server]` functions are `async` on the client, where the call becomes a
/// `fetch`. This is the bridge.
///
/// ```ignore
/// on:click={
///     move |_| {
///         krab_client::spawn(async move {
///             let _ = add_task("from the island".to_string()).await;
///         });
///     }
/// }
/// ```
///
/// Before this existed, the reference application and the getting-started guide
/// both told users to write the `cfg` and the transport by hand:
///
/// ```ignore
/// #[cfg(target_arch = "wasm32")]
/// wasm_bindgen_futures::spawn_local(async move { … });
/// ```
///
/// which leaks the transport into application code and does not compile off
/// wasm32.
///
/// # Scope
///
/// This is a browser task spawner, and it lives in `krab_client` because that
/// crate is browser-only by construction. It is deliberately **not** in
/// `krab_core`: the future here captures signals and is therefore `!Send`, and
/// inventing a native spawning story for a `!Send` future — a `LocalSet`, or a
/// silent no-op — would be a worse answer than not offering one.
///
/// For richer needs (pending state, error handling, cancellation) this is the
/// primitive that `Action`-style async state is built on.
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub fn spawn<F>(future: F)
where
    F: std::future::Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

// Function type for creating a component from JSON props
pub type ComponentFactory = fn(props_json: String) -> Node;

pub struct IslandDefinition {
    pub name: &'static str,
    pub factory: ComponentFactory,
}

// Register the inventory
inventory::collect!(IslandDefinition);

#[wasm_bindgen(start)]
pub fn start() {
    console::log_1(&"Krab Client initialized".into());
}
