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
//! // Entry point invoked from the page once the module is instantiated.
//! #[wasm_bindgen(start)]
//! pub fn main() {
//!     hydrate();
//! }
//! ```
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
#[cfg(feature = "web")]
use std::cell::{Cell, RefCell};
#[cfg(feature = "web")]
use std::collections::{HashMap, VecDeque};
#[cfg(feature = "web")]
// Only the non-wasm32 island error boundary uses these; on wasm32 the target is
// `panic = "abort"` and the boundary is deliberately absent. See `hydrate_island`.
#[cfg(not(target_arch = "wasm32"))]
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(feature = "web")]
use std::rc::Rc;
use wasm_bindgen::prelude::*;
#[cfg(feature = "web")]
use wasm_bindgen::JsCast;
use web_sys::console;
#[cfg(feature = "web")]
use web_sys::{Element, Node as WebNode, NodeList};

#[cfg(feature = "web")]
type EventClosure = Closure<dyn FnMut(web_sys::Event)>;
#[cfg(feature = "web")]
type EventClosureMap = std::collections::HashMap<u32, Vec<(String, EventClosure)>>;
#[cfg(feature = "web")]
type DynamicRegionMap = std::collections::HashMap<u32, DynamicRegionCells>;

/// Reports what happened at a boundary. Read to decide whether a boundary has
/// already been hydrated, written on the way through, and cleared by
/// [`unmount`].
#[cfg(feature = "web")]
const BOUNDARY_STATE_ATTR: &str = "data-krab-boundary-state";

/// The value `#[island]` stamps on server-rendered markup: "rendered, never
/// hydrated". Every other value on this attribute was written by a client that
/// already claimed the boundary.
#[cfg(feature = "web")]
const BOUNDARY_STATE_SSR: &str = "ssr";

/// Whether a boundary has already been claimed by a hydration pass.
///
/// The attribute's *presence* cannot answer this: `#[island]` ships
/// `data-krab-boundary-state="ssr"` from the server, so presence is the normal
/// state of un-hydrated markup. Only a value other than `ssr` means a client
/// has been here — `hydrating`, the terminal states this crate writes, or
/// `minimal_js` from the no-WASM escape hatch, whose islands already carry
/// hand-wired listeners and must not be hydrated over either.
#[cfg(feature = "web")]
fn boundary_is_hydrated(element: &Element) -> bool {
    element
        .get_attribute(BOUNDARY_STATE_ATTR)
        .is_some_and(|state| state != BOUNDARY_STATE_SSR)
}

// Holds event-listener closures for the lifetime of the page or node so they are not
// dropped (which would invalidate the JS function pointer) but also not
// silently leaked via forget().
#[cfg(feature = "web")]
thread_local! {
    static EVENT_CLOSURES: RefCell<EventClosureMap> = RefCell::new(EventClosureMap::new());
    static NEXT_DOM_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
    /// Every live [`Node::Dynamic`] region, keyed by the `__krab_region`
    /// expando on its anchor comment. A parent that tracks a nested region by
    /// its anchor uses this to reach the region's *content* and its effect —
    /// the anchor alone cannot reach either.
    static DYNAMIC_REGIONS: RefCell<DynamicRegionMap> = RefCell::new(DynamicRegionMap::new());
}

pub use krab_core::signal::*;

pub mod components;
// The module stays (its items are individually `cfg`'d, so a file-level
// `#![cfg]` would break this re-export), but with `demo-islands` off it is
// empty and the glob has nothing to import.
#[cfg(feature = "demo-islands")]
pub use components::*;

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

#[cfg(any(feature = "web", test))]
fn node_attribute_value<'a>(node: &'a Node, name: &str) -> Option<&'a str> {
    let Node::Element(element) = node else {
        return None;
    };

    element
        .attributes
        .iter()
        .find(|attr| attr.name == name)
        .map(|attr| attr.value.as_str())
}

#[cfg(any(feature = "web", test))]
fn boundary_state_from_factory_node(node: &Node) -> Option<&str> {
    node_attribute_value(node, "data-krab-boundary-state")
}

#[cfg(feature = "web")]
fn node_hydration_id(node: &Node) -> Option<&str> {
    node_attribute_value(node, krab_core::HYDRATION_NODE_ID_ATTR)
}

#[cfg(any(feature = "web", test))]
fn classify_boundary_state(factory_state: Option<&str>, mismatch_count: u32) -> &'static str {
    match factory_state {
        Some("decode-error") => "decode-error",
        Some("error") => "error",
        _ if mismatch_count > 0 => "patched",
        _ => "ok",
    }
}

#[cfg(feature = "web")]
#[derive(Debug, Clone)]
struct HydrationBoundary {
    island: String,
    id: String,
}

#[cfg(feature = "web")]
#[derive(Debug, Clone, Copy, Default)]
struct HydrationStats {
    consumed: u32,
    replacements: u32,
    appends: u32,
    removals: u32,
    reorders: u32,
    text_patches: u32,
    /// A node whose `data-krab-node-id` disagreed with the expected path was
    /// reused anyway (tag matched, no better candidate). Counted so the
    /// boundary reports `patched` rather than a clean `ok` over a mis-wired
    /// subtree — previously this was logged but invisible to the attributes
    /// monitoring reads.
    marker_mismatches: u32,
}

#[cfg(feature = "web")]
impl HydrationStats {
    fn with_consumed(mut self, consumed: u32) -> Self {
        self.consumed = consumed;
        self
    }

    fn merge(&mut self, other: Self) {
        self.consumed += other.consumed;
        self.replacements += other.replacements;
        self.appends += other.appends;
        self.removals += other.removals;
        self.reorders += other.reorders;
        self.text_patches += other.text_patches;
        self.marker_mismatches += other.marker_mismatches;
    }

    fn mismatch_count(self) -> u32 {
        self.replacements
            + self.appends
            + self.removals
            + self.reorders
            + self.text_patches
            + self.marker_mismatches
    }
}

#[cfg(feature = "web")]
pub fn log_hydration_diagnostic(scope: &str, name: &str, detail: &str) {
    let payload = serde_json::json!({
        "scope": scope,
        "island": name,
        "detail": detail,
    })
    .to_string();
    console::error_1(&payload.into());
}

#[cfg(feature = "web")]
fn log_hydration_boundary_diagnostic(
    level: &str,
    scope: &str,
    boundary: &HydrationBoundary,
    reason: &str,
    detail: &str,
    path: Option<&str>,
) {
    let payload = serde_json::json!({
        "scope": scope,
        "island": boundary.island,
        "boundary_id": boundary.id,
        "reason": reason,
        "detail": detail,
        "path": path,
    })
    .to_string();

    match level {
        "error" => console::error_1(&payload.into()),
        _ => console::warn_1(&payload.into()),
    }
}

#[cfg(feature = "web")]
fn describe_dom_node(node: &WebNode) -> String {
    if node.node_type() == 3 {
        return format!("#text({})", node.text_content().unwrap_or_default());
    }

    if let Some(element) = node.dyn_ref::<Element>() {
        return format!("<{}>", element.tag_name().to_lowercase());
    }

    format!("node_type:{}", node.node_type())
}

#[cfg(feature = "web")]
fn dom_hydration_id(node: &WebNode) -> Option<String> {
    node.dyn_ref::<Element>()
        .and_then(|element| element.get_attribute(krab_core::HYDRATION_NODE_ID_ATTR))
}

#[cfg(feature = "web")]
fn child_path(parent_path: &str, child_index: usize) -> String {
    if parent_path.is_empty() {
        child_index.to_string()
    } else {
        format!("{parent_path}.{child_index}")
    }
}

#[cfg(feature = "web")]
fn extra_dom_path(parent_path: &str, child_index: u32) -> String {
    if parent_path.is_empty() {
        format!("extra@{child_index}")
    } else {
        format!("{parent_path}.extra@{child_index}")
    }
}

#[cfg(feature = "web")]
fn find_dom_node_with_hydration_id(
    node_list: &NodeList,
    start_index: u32,
    expected_id: &str,
) -> Option<u32> {
    (start_index..node_list.length()).find(|index| {
        node_list
            .item(*index)
            .and_then(|node| dom_hydration_id(&node))
            .as_deref()
            == Some(expected_id)
    })
}

#[cfg(feature = "web")]
fn realign_node_by_hydration_id(
    parent: &WebNode,
    node_list: &NodeList,
    index: u32,
    expected_id: &str,
    boundary: &HydrationBoundary,
    path: &str,
) -> (Option<WebNode>, HydrationStats) {
    let current_node = node_list.item(index);

    if current_node.as_ref().and_then(dom_hydration_id).as_deref() == Some(expected_id) {
        return (current_node, HydrationStats::default());
    }

    if let Some(found_index) = find_dom_node_with_hydration_id(node_list, index + 1, expected_id) {
        let Some(found_node) = node_list.item(found_index) else {
            return (current_node, HydrationStats::default());
        };

        if let Err(err) = parent.insert_before(&found_node, current_node.as_ref()) {
            console::error_1(
                &format!(
                    "{{\"scope\":\"hydrate_recursive\",\"detail\":\"insert_before failed\",\"error\":\"{:?}\"}}",
                    err
                )
                .into(),
            );
            return (current_node, HydrationStats::default());
        }

        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate_recursive",
            boundary,
            "marker_reordered_dom_node",
            &format!(
                "moved node with hydration marker {} from DOM index {} to {}",
                expected_id, found_index, index
            ),
            Some(path),
        );

        return (
            Some(found_node),
            HydrationStats {
                reorders: 1,
                ..HydrationStats::default()
            },
        );
    }

    if let Some(current_node) = current_node {
        if let Some(actual_id) = dom_hydration_id(&current_node) {
            log_hydration_boundary_diagnostic(
                "warn",
                "hydrate_recursive",
                boundary,
                "element_marker_mismatch",
                &format!(
                    "expected hydration marker {} but found {} at DOM index {}",
                    expected_id, actual_id, index
                ),
                Some(path),
            );
            return (
                Some(current_node),
                HydrationStats {
                    marker_mismatches: 1,
                    ..HydrationStats::default()
                },
            );
        }
    }

    (node_list.item(index), HydrationStats::default())
}

/// The island name → factory lookup, built once per hydration pass.
///
/// `inventory::iter` walks a runtime linked list, so resolving each boundary
/// against it directly cost one linear scan *per island*: `n` islands against
/// `m` registered definitions meant `n × m` string comparisons on every load.
#[cfg(feature = "web")]
fn island_factories() -> HashMap<&'static str, ComponentFactory> {
    inventory::iter::<IslandDefinition>
        .into_iter()
        .map(|definition| (definition.name, definition.factory))
        .collect()
}

/// Hydrate one island boundary against the DOM element the server rendered it
/// into.
///
/// `ordinal` is only used to name a boundary whose markup predates
/// `data-krab-boundary-id`.
#[cfg(feature = "web")]
fn hydrate_island(
    element: &Element,
    ordinal: u32,
    factories: &HashMap<&'static str, ComponentFactory>,
) {
    // Idempotence. This function moves the boundary off `ssr` before anything
    // else happens, so a non-`ssr` state means the boundary already owns a
    // generation of event closures and region effects. Hydrating over it would
    // bind a second copy of every handler and orphan the first — the leak that
    // made callers strip `data-island` by hand after hydrating. `unmount`
    // clears the state, which is how a re-hydration is requested.
    if boundary_is_hydrated(element) {
        return;
    }

    let Some(name) = element.get_attribute("data-island") else {
        console::warn_1(&format!("Island element at index {ordinal} missing data-island").into());
        return;
    };

    let boundary_id_attr = element.get_attribute("data-krab-boundary-id");
    let boundary = HydrationBoundary {
        island: name.clone(),
        id: boundary_id_attr
            .clone()
            .unwrap_or_else(|| format!("{name}:legacy-{ordinal}")),
    };
    if boundary_id_attr.is_none() {
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate",
            &boundary,
            "missing_boundary_id",
            "server markup missing data-krab-boundary-id; using DOM index fallback",
            None,
        );
    }

    let _ = element.set_attribute("data-krab-boundary", &boundary.island);
    let _ = element.set_attribute("data-krab-boundary-id", &boundary.id);
    let _ = element.set_attribute("data-krab-boundary-mismatches", "0");
    let _ = element.set_attribute(BOUNDARY_STATE_ATTR, "hydrating");

    let props_json = element
        .get_attribute("data-props")
        .unwrap_or_else(|| "{}".to_string());

    let Some(factory) = factories.get(name.as_str()).copied() else {
        let _ = element.set_attribute(BOUNDARY_STATE_ATTR, "missing-definition");
        log_hydration_boundary_diagnostic(
            "error",
            "hydrate",
            &boundary,
            "missing_island_definition",
            "no registered island definition found",
            None,
        );
        return;
    };

    #[cfg(feature = "debug")]
    console::log_1(&format!("Hydrating island: {name}").into());

    // A panicking island factory **cannot** be contained on wasm32, and this
    // deliberately does not pretend otherwise. `wasm32-unknown-unknown` is
    // `panic = "abort"` (`rustc --print cfg` confirms it), so `catch_unwind`
    // never returns `Err`: the panic hook runs, then `unreachable` traps the
    // module and the whole hydration pass dies — every island later in document
    // order is left at `data-krab-boundary-state="ssr"`, unhydrated and inert,
    // with no diagnostic. The instance stays callable afterwards, so the page
    // looks fine. `tests/panic_boundary_browser.rs` demonstrates all of this.
    //
    // A `catch_unwind` here previously implied a recovery that never happened.
    // Restoring real per-island isolation requires a JS-side `try`/`catch`
    // around a per-island entry point, since only the JS boundary can observe a
    // wasm trap; that is tracked separately.
    #[cfg(target_arch = "wasm32")]
    let node = factory(props_json);

    // Off wasm32 — host builds that pick up `web` through workspace feature
    // unification — unwinding is real, so the boundary genuinely contains a
    // panicking factory and the fallback below is reachable.
    #[cfg(not(target_arch = "wasm32"))]
    let Ok(node) = catch_unwind(AssertUnwindSafe(|| factory(props_json))) else {
        log_hydration_boundary_diagnostic(
            "error",
            "hydrate",
            &boundary,
            "factory_panic",
            "factory panic captured",
            None,
        );
        let _ = element.set_attribute(BOUNDARY_STATE_ATTR, "error");
        element.set_inner_html("<div role=\"alert\">Hydration fallback rendered.</div>");
        return;
    };

    let node = krab_core::annotate_hydration_tree(node, &boundary.id);
    let factory_state = boundary_state_from_factory_node(&node);
    let stats = hydrate_node(WebNode::from(element.clone()), &node, &boundary);
    let mismatch_count = stats.mismatch_count();

    let _ = element.set_attribute("data-krab-boundary-mismatches", &mismatch_count.to_string());
    let _ = element.set_attribute(
        BOUNDARY_STATE_ATTR,
        classify_boundary_state(factory_state, mismatch_count),
    );

    if mismatch_count > 0 {
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate",
            &boundary,
            "boundary_patched",
            &format!(
                "patched DOM during hydration (replacements={}, appends={}, removals={}, reorders={}, text_patches={})",
                stats.replacements,
                stats.appends,
                stats.removals,
                stats.reorders,
                stats.text_patches
            ),
            None,
        );
    }
}

/// Hydrate only the islands inside `root`. Skips boundaries already hydrated.
///
/// `root` itself is hydrated too when it carries `data-island`, so a caller
/// holding a single island element does not have to reach for its parent.
///
/// "Already hydrated" means `data-krab-boundary-state` has moved off the `ssr`
/// value the server stamps. Calling this twice over the same subtree is
/// therefore a no-op the second time rather than a double binding of every
/// handler. [`unmount`] clears the attribute, so `unmount` followed by
/// `hydrate_within` is the supported way to ask for a genuine re-hydration.
#[cfg(feature = "web")]
pub fn hydrate_within(root: &web_sys::Element) {
    // One map for the whole pass, not one scan per boundary.
    let factories = island_factories();
    let mut ordinal = 0u32;

    if root.has_attribute("data-island") {
        hydrate_island(root, ordinal, &factories);
        ordinal += 1;
    }

    let islands = match root.query_selector_all("[data-island]") {
        Ok(nodes) => nodes,
        Err(err) => {
            console::error_1(
                &format!(
                    "{{\"scope\":\"hydrate\",\"detail\":\"query_selector_all failed\",\"error\":\"{err:?}\"}}"
                )
                .into(),
            );
            return;
        }
    };

    for index in 0..islands.length() {
        let Some(node) = islands.item(index) else {
            console::warn_1(&format!("Missing island element at index {index}").into());
            continue;
        };
        let Ok(element) = node.dyn_into::<Element>() else {
            console::warn_1(&format!("Island node at index {index} is not an Element").into());
            continue;
        };
        hydrate_island(&element, ordinal, &factories);
        ordinal += 1;
    }
}

/// Release every runtime resource held by `root`'s subtree: event closures,
/// dynamic-region registrations, and region effects. Does not remove `root`.
///
/// Call this before discarding a hydrated subtree. Dropping the DOM alone is
/// not enough: the event closures are owned by this crate (they have to be —
/// dropping one invalidates the JS function pointer behind a live listener),
/// and a dynamic region's effect stays subscribed to its signals, re-rendering
/// detached nodes for the life of the page.
///
/// It also clears `data-krab-boundary-state` from `root` and every boundary
/// beneath it, so the subtree is eligible for [`hydrate_within`] again.
///
/// # Limitation
///
/// A dynamic region that has not yet re-rendered has no anchor comment in the
/// DOM and so cannot be found by a subtree walk; its effect is released when
/// its enclosing region re-renders or is removed. Regions that have rendered at
/// least once — the ones that own DOM — are always released here.
#[cfg(feature = "web")]
pub fn unmount(root: &web_sys::Element) {
    release_dom_node_resources(root.as_ref());

    let _ = root.remove_attribute(BOUNDARY_STATE_ATTR);
    let Ok(boundaries) = root.query_selector_all(&format!("[{BOUNDARY_STATE_ATTR}]")) else {
        return;
    };
    for index in 0..boundaries.length() {
        let Some(element) = boundaries
            .item(index)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        let _ = element.remove_attribute(BOUNDARY_STATE_ATTR);
    }
}

/// Number of DOM elements currently holding retained event closures.
#[cfg(feature = "web")]
#[doc(hidden)]
pub fn event_closure_count() -> usize {
    EVENT_CLOSURES.with(|closures| closures.borrow().len())
}

/// Number of dynamic regions currently registered against a live anchor.
#[cfg(feature = "web")]
#[doc(hidden)]
pub fn dynamic_region_count() -> usize {
    DYNAMIC_REGIONS.with(|regions| regions.borrow().len())
}

/// Hydrate every island in the document.
///
/// Delegates to [`hydrate_within`] over the document element, so the two share
/// one implementation and one idempotence rule: a boundary whose
/// `data-krab-boundary-state` has moved off `ssr` is left alone.
#[wasm_bindgen]
pub fn hydrate() {
    #[cfg(feature = "debug")]
    console::log_1(&"Hydrating Krab app...".into());

    #[cfg(feature = "web")]
    {
        let Some(window) = web_sys::window() else {
            console::error_1(&"{\"scope\":\"hydrate\",\"detail\":\"missing window\"}".into());
            return;
        };
        let Some(document) = window.document() else {
            console::error_1(&"{\"scope\":\"hydrate\",\"detail\":\"missing document\"}".into());
            return;
        };
        let Some(root) = document.document_element() else {
            console::error_1(
                &"{\"scope\":\"hydrate\",\"detail\":\"missing document element\"}".into(),
            );
            return;
        };

        hydrate_within(&root);
    }
}

#[cfg(feature = "web")]
fn hydrate_node(real_node: WebNode, v_node: &Node, boundary: &HydrationBoundary) -> HydrationStats {
    // The v_node corresponds to the content of the real_node (the wrapper div)
    hydrate_children(&real_node, std::slice::from_ref(v_node), boundary, "")
}

#[cfg(feature = "web")]
fn hydrate_children(
    parent: &WebNode,
    v_nodes: &[Node],
    boundary: &HydrationBoundary,
    parent_path: &str,
) -> HydrationStats {
    let child_nodes = parent.child_nodes();
    let mut dom_index = 0;
    let mut stats = HydrationStats::default();
    for (child_index, v_node) in v_nodes.iter().enumerate() {
        let path = child_path(parent_path, child_index);
        let child_stats =
            hydrate_recursive(parent, &child_nodes, dom_index, v_node, boundary, &path);
        dom_index += child_stats.consumed;
        stats.merge(child_stats);
    }

    while child_nodes.length() > dom_index {
        let Some(extra_node) = child_nodes.item(dom_index) else {
            break;
        };
        let path = extra_dom_path(parent_path, dom_index);
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate_recursive",
            boundary,
            "unexpected_dom_node",
            "removing extra DOM node that was not present in the expected tree",
            Some(&path),
        );
        release_dom_node_resources(&extra_node);
        if let Err(err) = parent.remove_child(&extra_node) {
            console::error_1(
                &format!(
                    "{{\"scope\":\"hydrate_recursive\",\"detail\":\"remove_child failed\",\"error\":\"{:?}\"}}",
                    err
                )
                .into(),
            );
            break;
        }
        stats.removals += 1;
    }

    stats
}

/// Replace `old` with a freshly built node for `v_node`, releasing everything
/// `old`'s subtree retains first so it is not leaked.
#[cfg(feature = "web")]
fn replace_dom_node(parent: &WebNode, old: &WebNode, v_node: &Node, scope: &str) {
    let Some(new_node) = create_dom_node(v_node) else {
        return;
    };

    release_dom_node_resources(old);
    if let Err(err) = parent.replace_child(&new_node, old) {
        console::error_1(
            &format!(
                "{{\"scope\":\"{scope}\",\"detail\":\"replace_child failed\",\"error\":\"{err:?}\"}}"
            )
            .into(),
        );
    }
}

/// Append a freshly built node for `v_node` to `parent`.
#[cfg(feature = "web")]
fn append_dom_node(parent: &WebNode, v_node: &Node, scope: &str) {
    let Some(new_node) = create_dom_node(v_node) else {
        return;
    };

    if let Err(err) = parent.append_child(&new_node) {
        console::error_1(
            &format!(
                "{{\"scope\":\"{scope}\",\"detail\":\"append_child failed\",\"error\":\"{err:?}\"}}"
            )
            .into(),
        );
    }
}

/// Detach every listener previously attached by [`attach_element_events`].
///
/// The JS listener is removed *before* its closure is dropped — the reverse
/// order leaves a live listener whose Rust side is gone, which throws
/// "closure invoked after being dropped" on the next event.
#[cfg(feature = "web")]
fn detach_element_events(real_el: &Element) {
    let Some(id) = node_expando_id(real_el.as_ref(), "__krab_id") else {
        return;
    };
    let Some(pairs) = EVENT_CLOSURES.with(|closures| closures.borrow_mut().remove(&id)) else {
        return;
    };
    for (name, closure) in &pairs {
        let _ = real_el.remove_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
    }
}

/// Bind `v_el`'s event listeners to a reused DOM element.
///
/// Closures are stashed in `EVENT_CLOSURES` under a per-element `__krab_id`
/// rather than `forget()`ed: dropping them would invalidate the JS function
/// pointer, and forgetting them would leak on every re-render.
///
/// Any binding already on the element is detached first. Without that, a second
/// hydration pass over the same element allocated a *fresh* `__krab_id` and
/// overwrote the expando, orphaning the previous generation of closures in
/// `EVENT_CLOSURES` while their JS listeners stayed bound — so every handler
/// fired twice and neither generation could ever be released. Detaching here
/// rather than at each call site makes "attach" mean "these are now the
/// element's listeners", which is the only invariant the callers actually want.
#[cfg(feature = "web")]
fn attach_element_events(real_el: &Element, v_el: &krab_core::Element, scope: &str) {
    detach_element_events(real_el);

    let mut node_closures = Vec::new();

    for event in &v_el.events {
        let name = event.name.clone();
        let callback = event.callback.clone();

        let closure =
            Closure::wrap(Box::new(move |e: web_sys::Event| callback(e)) as Box<dyn FnMut(_)>);

        if let Err(err) =
            real_el.add_event_listener_with_callback(&name, closure.as_ref().unchecked_ref())
        {
            console::error_1(
                &format!(
                    "{{\"scope\":\"{scope}\",\"detail\":\"failed to attach event listener\",\"event\":\"{name}\",\"error\":\"{err:?}\"}}"
                )
                .into(),
            );
        }
        node_closures.push((name, closure));
    }

    if node_closures.is_empty() {
        return;
    }

    let id = NEXT_DOM_ID.with(|id| {
        let v = id.get();
        id.set(v + 1);
        v
    });
    let _ = js_sys::Reflect::set(
        real_el.as_ref(),
        &JsValue::from_str("__krab_id"),
        &JsValue::from_f64(id as f64),
    );
    EVENT_CLOSURES.with(|v| v.borrow_mut().insert(id, node_closures));
}

/// Hydrate one `Node::Element` against the DOM child at `index`.
///
/// Three outcomes: reuse the node and recurse into its children, replace it
/// when the tags disagree, or append when the DOM ran out of children.
#[cfg(feature = "web")]
fn hydrate_element(
    parent: &WebNode,
    node_list: &NodeList,
    index: u32,
    v_node: &Node,
    v_el: &krab_core::Element,
    boundary: &HydrationBoundary,
    path: &str,
) -> HydrationStats {
    // A hydration marker lets a moved node be found at another index rather
    // than destroyed and rebuilt.
    let (aligned_node_opt, alignment_stats) = match node_hydration_id(v_node) {
        Some(expected_id) => {
            realign_node_by_hydration_id(parent, node_list, index, expected_id, boundary, path)
        }
        None => (node_list.item(index), HydrationStats::default()),
    };

    let Some(real_node) = aligned_node_opt else {
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate_recursive",
            boundary,
            "missing_dom_node",
            &format!(
                "expected <{}> but DOM child was missing; appending",
                v_el.tag
            ),
            Some(path),
        );
        append_dom_node(parent, v_node, "hydrate_recursive");

        let mut stats = HydrationStats {
            consumed: 1,
            appends: 1,
            ..HydrationStats::default()
        };
        stats.merge(alignment_stats);
        return stats;
    };

    // `Element::tag_name` is uppercase for HTML elements, so this comparison
    // must stay case-insensitive.
    let matching_el = real_node
        .dyn_ref::<Element>()
        .filter(|real_el| real_el.tag_name().eq_ignore_ascii_case(&v_el.tag));

    if let Some(real_el) = matching_el {
        attach_element_events(real_el, v_el, "hydrate_recursive");
        let mut stats =
            hydrate_children(&real_node, &v_el.children, boundary, path).with_consumed(1);
        stats.merge(alignment_stats);
        return stats;
    }

    log_hydration_boundary_diagnostic(
        "warn",
        "hydrate_recursive",
        boundary,
        "element_tag_mismatch",
        &format!(
            "expected <{}> but found {} at DOM index {}",
            v_el.tag,
            describe_dom_node(&real_node),
            index
        ),
        Some(path),
    );
    replace_dom_node(parent, &real_node, v_node, "hydrate_recursive");

    let mut stats = HydrationStats {
        consumed: 1,
        replacements: 1,
        ..HydrationStats::default()
    };
    stats.merge(alignment_stats);
    stats
}

/// Hydrate one `Node::Text` against the DOM child at `index`.
///
/// Patching a text node in place, rather than replacing it, is what keeps a
/// selection or an IME composition alive across hydration.
#[cfg(feature = "web")]
fn hydrate_text(
    parent: &WebNode,
    real_node_opt: Option<WebNode>,
    v_node: &Node,
    text: &str,
    boundary: &HydrationBoundary,
    path: &str,
) -> HydrationStats {
    /// `Node.TEXT_NODE`
    const TEXT_NODE: u16 = 3;

    let Some(real_node) = real_node_opt else {
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate_recursive",
            boundary,
            "missing_dom_node",
            &format!("expected text {text:?} but DOM child was missing; appending"),
            Some(path),
        );
        append_dom_node(parent, v_node, "hydrate_recursive");

        return HydrationStats {
            consumed: 1,
            appends: 1,
            ..HydrationStats::default()
        };
    };

    if real_node.node_type() != TEXT_NODE {
        log_hydration_boundary_diagnostic(
            "warn",
            "hydrate_recursive",
            boundary,
            "expected_text_found_non_text_node",
            &format!(
                "expected text {:?} but found {}; replacing node",
                text,
                describe_dom_node(&real_node)
            ),
            Some(path),
        );
        replace_dom_node(parent, &real_node, v_node, "hydrate_recursive");

        return HydrationStats {
            consumed: 1,
            replacements: 1,
            ..HydrationStats::default()
        };
    }

    let actual = real_node.text_content().unwrap_or_default();
    if actual == text {
        return HydrationStats {
            consumed: 1,
            ..HydrationStats::default()
        };
    }

    log_hydration_boundary_diagnostic(
        "warn",
        "hydrate_recursive",
        boundary,
        "text_content_mismatch",
        &format!("expected text {text:?} but found {actual:?}; patching text node"),
        Some(path),
    );
    real_node.set_text_content(Some(text));

    HydrationStats {
        consumed: 1,
        text_patches: 1,
        ..HydrationStats::default()
    }
}

#[cfg(feature = "web")]
fn hydrate_recursive(
    parent: &WebNode,
    node_list: &NodeList,
    index: u32,
    v_node: &Node,
    boundary: &HydrationBoundary,
    path: &str,
) -> HydrationStats {
    match v_node {
        Node::Element(v_el) => {
            hydrate_element(parent, node_list, index, v_node, v_el, boundary, path)
        }
        // `item` is resolved here rather than up front: it crosses the JS
        // boundary, and this is the only arm that consumes it. The element arm
        // does its own lookup (possibly at a realigned index), and the fragment
        // and dynamic arms never touch the node at `index` directly.
        Node::Text(text) => {
            hydrate_text(parent, node_list.item(index), v_node, text, boundary, path)
        }
        Node::Fragment(children) => {
            let mut consumed = 0;
            let mut stats = HydrationStats::default();
            for (child_index, child) in children.iter().enumerate() {
                let child_stats = hydrate_recursive(
                    parent,
                    node_list,
                    index + consumed,
                    child,
                    boundary,
                    &child_path(path, child_index),
                );
                consumed += child_stats.consumed;
                stats.merge(child_stats);
            }
            stats
        }
        Node::Dynamic(f) => {
            let cells = DynamicRegionCells {
                rendered: Rc::new(RefCell::new(Vec::new())),
                current_vnode: Rc::new(RefCell::new(None)),
                // Created on first *update*, not now: inserting a node during
                // the hydration traversal would shift the live `NodeList` and
                // report every following sibling as a mismatch.
                anchor: Rc::new(RefCell::new(None)),
                empty_position: Rc::new(RefCell::new(None)),
                effect: Rc::new(RefCell::new(None)),
            };

            // The initial hydration runs *inside* the effect's first run, so a
            // Dynamic nested in the SSR content is set up while this one is
            // current and can therefore be handed to it for disposal (see
            // `own_region_effect_by_enclosing_effect`). Hydrating first and
            // creating the effect afterwards left every nested region's effect
            // in `ROOT_EFFECTS`: immortal, subscribed, and re-rendering
            // detached DOM for the life of the page.
            // `Cell`, not `RefCell`: `HydrationStats` is `Copy`, and reading it
            // back as this arm's value must not leave a borrow guard alive
            // longer than the cell it came from.
            let stats_cell: Rc<Cell<HydrationStats>> =
                Rc::new(Cell::new(HydrationStats::default()));

            let f = f.clone();
            let first_run = Rc::new(Cell::new(true));
            let effect_cells = cells.clone();
            let parent = parent.clone();
            let node_list = node_list.clone();
            let boundary = boundary.clone();
            let path = path.to_string();
            let stats_for_effect = stats_cell.clone();

            let handle = create_effect_scoped(move || {
                let new_v_node = f();

                if first_run.get() {
                    first_run.set(false);

                    let stats = hydrate_recursive(
                        &parent,
                        &node_list,
                        index,
                        &new_v_node,
                        &boundary,
                        &path,
                    );

                    // A `Dynamic` owns a *run* of siblings, not one node - a
                    // `<For>` hydrates one node per row. `stats.consumed` is
                    // exactly how many DOM nodes this vnode claimed.
                    *effect_cells.rendered.borrow_mut() = (0..stats.consumed)
                        .filter_map(|offset| node_list.item(index + offset))
                        .collect();

                    // Captured while the node list is still aligned: where a
                    // lazily-created anchor belongs when the run is empty.
                    // Without this, an empty SSR render (a `<For>` over an
                    // empty list, `<Show when=false>`) had no reference point
                    // and the region could never display anything.
                    *effect_cells.empty_position.borrow_mut() = parent
                        .dyn_ref::<Element>()
                        .cloned()
                        .map(|el| (el, node_list.item(index + stats.consumed)));

                    *effect_cells.current_vnode.borrow_mut() = Some(new_v_node);
                    stats_for_effect.set(stats);
                    return;
                }

                update_dynamic_region(&effect_cells, new_v_node);
            });

            // Stored so the region's effect dies with the region rather than
            // outliving it in `ROOT_EFFECTS`. Safe to assign after the fact:
            // the first run has already completed synchronously, and only
            // teardown reads this slot.
            *cells.effect.borrow_mut() = Some(handle);
            own_region_effect_by_enclosing_effect(&cells);

            // The effect ran synchronously, so the stats are populated.
            stats_cell.get()
        }
    }
}

/// Read a `u32` expando written by [`attach_element_events`] or
/// [`register_dynamic_region`]. One reader for both, so the two registries
/// cannot drift apart in how they identify a node.
#[cfg(feature = "web")]
fn node_expando_id(target: &JsValue, key: &str) -> Option<u32> {
    js_sys::Reflect::get(target, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
        .map(|value| value as u32)
}

/// Release every runtime resource `node`'s subtree holds, without removing any
/// of it: event listeners and their closures, and the registration and effect
/// of any dynamic region anchored inside it.
///
/// One walk covers both registries. Previously this released `__krab_id`
/// closures only, so a replaced or removed subtree left every `__krab_region`
/// entry it contained in `DYNAMIC_REGIONS` forever — the map only ever shrank
/// on the one path that happened to call `remove_region_content`.
///
/// Listeners go through [`detach_element_events`] rather than a bare map
/// removal. For a subtree on its way out of the document the difference is
/// invisible, but [`unmount`] deliberately *leaves* its subtree in place:
/// dropping a closure whose listener is still bound makes the next event throw
/// "closure invoked after being dropped".
///
/// A region *anchored* in this subtree has its rendered run in the subtree too
/// (the run is the anchor's preceding siblings), so the caller's own DOM removal
/// disposes of the nodes; unregistering is all that is needed here. An anchor
/// whose content lies *outside* the discarded subtree — the anchor itself being
/// replaced — is the separate job of [`remove_region_content`].
#[cfg(feature = "web")]
fn release_dom_node_resources(node: &WebNode) {
    if let Some(element) = node.dyn_ref::<Element>() {
        detach_element_events(element);
    }
    if let Some(region) = take_dynamic_region(node) {
        dispose_region_effect(&region);
    }

    let child_nodes = node.child_nodes();
    for index in 0..child_nodes.length() {
        if let Some(child) = child_nodes.item(index) {
            release_dom_node_resources(&child);
        }
    }
}

/// The state one [`Node::Dynamic`] region shares between its effect runs.
///
/// One definition serves both the hydration and the client-mount paths — the
/// two previously carried verbatim copies of the update logic, and the copies
/// had already diverged (the hydration copy could not create its anchor for an
/// initially-empty run, leaving the region permanently dead).
///
/// Every field is an `Rc` handle, so `Clone` shares state rather than copying
/// it — that is the point. The struct was hand-cloned field by field at both
/// construction sites, which meant adding a field silently produced two
/// regions whose new state was *not* shared.
#[cfg(feature = "web")]
#[derive(Clone)]
struct DynamicRegionCells {
    /// The tracked run: one node per flattened child, region anchors standing
    /// in for nested regions.
    rendered: Rc<RefCell<Vec<WebNode>>>,
    current_vnode: Rc<RefCell<Option<Node>>>,
    /// The trailing comment bounding the run. Pre-filled on the mount path;
    /// created on first update on the hydration path (inserting during the
    /// hydration traversal would shift the live `NodeList`).
    anchor: Rc<RefCell<Option<WebNode>>>,
    /// Where a lazily-created anchor belongs when the run is empty: the parent
    /// element and the sibling the run precedes. Without this, an empty SSR
    /// render had no reference point and the region could never show anything.
    empty_position: Rc<RefCell<Option<EmptyRunPosition>>>,
    /// The region's own effect, so it can be torn down when the region is.
    ///
    /// Filled in immediately after `create_effect_scoped` returns — the handle
    /// cannot exist before the closure that captures these cells does.
    ///
    /// This closes an `Rc` cycle (handle → effect state → closure → these
    /// cells → handle) that is broken by *taking* the handle out at disposal,
    /// which is why [`dispose_region_effect`] takes rather than borrows. A
    /// region that is never disposed keeps that cycle, exactly as a root effect
    /// was kept alive forever before.
    effect: Rc<RefCell<Option<EffectHandle>>>,
}

/// The parent element and the following sibling an empty run sits before.
#[cfg(feature = "web")]
type EmptyRunPosition = (Element, Option<WebNode>);

/// Apply one new render of a dynamic region to the DOM.
#[cfg(feature = "web")]
fn update_dynamic_region(cells: &DynamicRegionCells, new_v_node: Node) {
    // Taken out of the cells rather than borrowed across the update: the cells
    // are written back to below, and an outstanding shared borrow would make
    // that a panic. The vnode is *moved*, not cloned — every exit path of this
    // function overwrites the cell with `new_v_node`, so a clone here paid for
    // a deep copy of the entire previous tree only to discard it. The rendered
    // nodes are cheap `WebNode` handles, but still cloned only once.
    let previous_nodes = cells.rendered.borrow().clone();
    let previous_vnode = cells.current_vnode.borrow_mut().take();

    let anchor_node = {
        let existing = cells.anchor.borrow().clone();
        match existing {
            Some(anchor) => anchor,
            None => {
                let Some(document) = web_sys::window().and_then(|w| w.document()) else {
                    *cells.current_vnode.borrow_mut() = Some(new_v_node);
                    return;
                };
                let created: WebNode = document.create_comment("krab-dynamic").into();

                let inserted = if let Some(last) = previous_nodes.last() {
                    // After the run's current last node.
                    last.parent_node()
                        .map(|parent| {
                            let _ = parent.insert_before(&created, last.next_sibling().as_ref());
                        })
                        .is_some()
                } else if let Some((parent, next)) = cells.empty_position.borrow().as_ref() {
                    // Empty run: use the position captured at hydration. The
                    // captured next-sibling may have been replaced since; fall
                    // back to appending rather than guessing.
                    let next_ok = next
                        .as_ref()
                        .filter(|n| n.parent_node().as_deref() == Some(parent.as_ref()));
                    let _ = parent.insert_before(&created, next_ok);
                    true
                } else {
                    false
                };

                if !inserted {
                    // No position to anchor to: give up rather than guess.
                    *cells.current_vnode.borrow_mut() = Some(new_v_node);
                    return;
                }

                register_dynamic_region(&created, cells);
                *cells.anchor.borrow_mut() = Some(created.clone());
                created
            }
        }
    };

    let Some(parent) = anchor_node
        .parent_node()
        .and_then(|node| node.dyn_into::<Element>().ok())
    else {
        // Not mounted (or mounted under a non-element): nothing to update
        // against.
        *cells.current_vnode.borrow_mut() = Some(new_v_node);
        return;
    };

    let next = reconcile_range(
        &parent,
        previous_nodes,
        previous_vnode.as_slice(),
        std::slice::from_ref(&new_v_node),
        Some(&anchor_node),
    );

    match next {
        Some(nodes) => *cells.rendered.borrow_mut() = nodes,
        None => {
            // Reconciliation declined — rebuild the run wholesale, still
            // bounded by the anchor so siblings are untouched. `reconcile_range`
            // consumed the node list but never writes to the cells, so the cell
            // still holds the previous run; take it rather than keeping a
            // second clone alive for this path.
            let stale = std::mem::take(&mut *cells.rendered.borrow_mut());
            for node in &stale {
                remove_tracked_node(&parent, node);
            }

            let mut fresh_tracked = Vec::new();
            for (insert, tracked) in create_run_nodes(&new_v_node) {
                let _ = parent.insert_before(&insert, Some(&anchor_node));
                fresh_tracked.push(tracked);
            }
            *cells.rendered.borrow_mut() = fresh_tracked;
        }
    }

    *cells.current_vnode.borrow_mut() = Some(new_v_node);
}

#[cfg(feature = "web")]
fn patch_dom(
    _parent: &WebNode,
    real_node: &WebNode,
    old_vnode: &Node,
    new_vnode: &Node,
) -> Option<WebNode> {
    match (old_vnode, new_vnode) {
        (Node::Text(old_text), Node::Text(new_text)) => {
            if old_text != new_text {
                real_node.set_text_content(Some(new_text));
            }
            Some(real_node.clone())
        }
        (Node::Element(old_el), Node::Element(new_el)) if old_el.tag == new_el.tag => {
            let el = real_node.dyn_ref::<Element>()?;

            // Attributes
            for old_attr in &old_el.attributes {
                if !new_el.attributes.iter().any(|a| a.name == old_attr.name) {
                    let _ = el.remove_attribute(&old_attr.name);
                }
            }
            for new_attr in &new_el.attributes {
                let old_attr = old_el.attributes.iter().find(|a| a.name == new_attr.name);
                if old_attr.map(|a| &a.value) != Some(&new_attr.value) {
                    let _ = el.set_attribute(&new_attr.name, &new_attr.value);
                }
            }

            // Listeners: the reused node keeps whatever closures its *creation*
            // render captured unless they are swapped here. A <For> row patched
            // under a stable key, or a <Show> branch sharing a tag with its
            // sibling, would otherwise fire the old render's handler over the
            // old captured data while displaying the new content.
            if !old_el.events.is_empty() || !new_el.events.is_empty() {
                // `attach_element_events` detaches first, so this swaps the
                // generation even when the new render has no listeners at all.
                attach_element_events(el, new_el, "patch_dom");
            }

            patch_children(el, &old_el.children, &new_el.children)?;
            Some(real_node.clone())
        }
        _ => None,
    }
}

/// Flatten a child list into the sequence of nodes it actually produces.
///
/// A `Fragment` has no DOM node of its own — its children are siblings of
/// whatever surrounds it. Flattening first is what lets keys match across a
/// fragment boundary, which matters because a list rendered by a `Dynamic`
/// arrives as exactly that: a fragment of keyed elements among other children.
#[cfg(feature = "web")]
fn flatten_children<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
    for node in nodes {
        match node {
            Node::Fragment(children) => flatten_children(children, out),
            other => out.push(other),
        }
    }
}

/// The reconciliation key for a vnode, if it carries one.
///
/// Reuses `data-krab-node-id`, already stamped by `annotate_hydration_tree` and
/// already used by `realign_node_by_hydration_id` to move server-rendered nodes
/// into place. One key scheme, one meaning.
#[cfg(feature = "web")]
fn reconcile_key(node: &Node) -> Option<&str> {
    node_hydration_id(node)
}

/// Reconcile `el`'s children from `old_children` to `new_children`.
///
/// Returns `None` when the caller should rebuild instead.
///
/// Previously this bailed out — and so rebuilt the whole subtree — whenever the
/// child *count* changed, so adding one item to a list destroyed and recreated
/// every row, losing DOM identity, focus, and scroll position. Keyed children
/// are now matched and moved; unkeyed ones fall back to matching by position.
#[cfg(feature = "web")]
fn patch_children(el: &Element, old_children: &[Node], new_children: &[Node]) -> Option<()> {
    // Snapshot before mutating: the live NodeList shifts under every move.
    let child_nodes = el.child_nodes();
    let existing: Vec<WebNode> = (0..child_nodes.length())
        .filter_map(|index| child_nodes.item(index))
        .collect();

    reconcile_range(el, existing, old_children, new_children, None).map(|_| ())
}

/// Reconcile an explicit run of sibling nodes rather than all of `el`'s
/// children, and return the nodes the run now consists of.
///
/// `anchor` is the node new content is inserted before. A [`Node::Dynamic`]
/// owns a slice of its parent — a `<ul>` may hold a `<For>` alongside other
/// children — so it passes its own nodes and its trailing anchor, and only that
/// slice is touched.
///
/// The returned list is what the caller should track from now on. Deriving the
/// run from DOM positions instead is not possible once a parent holds more than
/// one dynamic region, which is why the caller tracks it explicitly.
#[cfg(feature = "web")]
fn reconcile_range(
    el: &Element,
    existing_nodes: Vec<WebNode>,
    old_children: &[Node],
    new_children: &[Node],
    anchor: Option<&WebNode>,
) -> Option<Vec<WebNode>> {
    let mut old_flat: Vec<&Node> = Vec::new();
    flatten_children(old_children, &mut old_flat);
    let mut new_flat: Vec<&Node> = Vec::new();
    flatten_children(new_children, &mut new_flat);

    let mut existing: Vec<Option<WebNode>> = existing_nodes.into_iter().map(Some).collect();

    // The flattened old vnodes line up 1:1 with the DOM nodes. If they do not,
    // something outside the reconciler changed the DOM and the safe answer is a
    // rebuild rather than a guess.
    if existing.len() != old_flat.len() {
        return None;
    }

    let mut consumed = vec![false; existing.len()];
    let mut placed: Vec<WebNode> = Vec::with_capacity(new_flat.len());

    // Keyed sources, resolved up front: key → source indices in first-to-last
    // order, drained from the front as they are claimed. This replaces a linear
    // scan of `old_flat` per new child — O(n×m) across an update, and every
    // probe re-scanned the attribute `Vec` inside `reconcile_key` — with one
    // pass here and an O(1) lookup per child. Only keyed old nodes enter the
    // map, and a keyed old node can only ever be consumed through it (the
    // positional fallback below refuses keyed sources), so front-to-back
    // draining claims exactly the first unconsumed match, duplicate keys
    // included — the same source the scan used to find.
    let mut keyed_sources: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (index, old) in old_flat.iter().enumerate() {
        if let Some(key) = reconcile_key(old) {
            keyed_sources.entry(key).or_default().push_back(index);
        }
    }

    // Where the run currently begins. Captured before any mutation and used as
    // the reference for the first node, so a node already in place is
    // recognised and left alone. Falls back to the anchor for an empty run.
    let run_start: Option<WebNode> = existing
        .first()
        .and_then(|node| node.clone())
        .or_else(|| anchor.cloned());

    for (target_index, new_child) in new_flat.iter().enumerate() {
        // A keyed node is matched wherever it moved to. An unkeyed one falls
        // back to the node at the same position, and only if that node is
        // itself unkeyed — otherwise a keyed node could be consumed by an
        // unrelated positional match and then rebuilt when its own key comes up.
        let keyed_source = reconcile_key(new_child)
            .and_then(|key| keyed_sources.get_mut(key))
            .and_then(VecDeque::pop_front);

        let source_index = keyed_source.or_else(|| {
            let positional = target_index;
            let free = positional < consumed.len() && !consumed[positional];
            let unkeyed = old_flat
                .get(positional)
                .is_some_and(|old| reconcile_key(old).is_none());

            (free && unkeyed).then_some(positional)
        });

        match source_index {
            Some(source) => {
                consumed[source] = true;
                let node = existing[source].clone()?;

                // Patch in place; if the shapes are incompatible, swap the node.
                if patch_dom(el, &node, old_flat[source], new_child).is_none() {
                    let (replacement, tracked) = create_tracked_child(new_child)?;
                    // A region anchor's content lives *beside* it: remove it
                    // first, or replacing the anchor strands the region's rows.
                    remove_region_content(el, &node);
                    release_dom_node_resources(&node);
                    el.replace_child(&replacement, &node).ok()?;
                    existing[source] = Some(tracked);
                }

                let node = existing[source].clone()?;
                place_before(el, &node, placed.last(), run_start.as_ref());
                placed.push(node);
            }
            None => {
                let (created, tracked) = create_tracked_child(new_child)?;
                place_before(el, &created, placed.last(), run_start.as_ref());
                placed.push(tracked);
            }
        }
    }

    // Anything the new list did not claim is gone — including the content of
    // any dynamic region whose anchor is the tracked node.
    for (index, node) in existing.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        if let Some(node) = node {
            remove_tracked_node(el, node);
        }
    }

    Some(placed)
}

/// Put `node` where it belongs: directly after `previous`, or at `run_start`
/// when it is the first of the run.
///
/// Positioning against the previously placed sibling keeps the run
/// self-contained — it never consults indices into the parent, so unrelated
/// siblings and other dynamic regions are untouched.
///
/// The no-op check is the load-bearing part. An unconditional `insert_before`
/// detaches and reattaches the node, discarding focus, selection, and any
/// running transition *inside* it — precisely what keyed reconciliation exists
/// to prevent. Passing the anchor as the first-node reference instead of
/// `run_start` reintroduces exactly that: the first node is never recognised as
/// already-correct, so every update re-inserts it and everything it contains.
#[cfg(feature = "web")]
fn place_before(
    el: &Element,
    node: &WebNode,
    previous: Option<&WebNode>,
    run_start: Option<&WebNode>,
) {
    let target = match previous {
        Some(previous) => previous.next_sibling(),
        None => run_start.cloned().or_else(|| el.first_child()),
    };

    if target.as_ref() == Some(node) {
        return;
    }

    let _ = el.insert_before(node, target.as_ref());
}

/// Build a real DOM node from a vnode.
///
/// Exposed only for the browser test suite, which has to drive the *actual*
/// builder and reconciler: the properties under test — node identity across a
/// re-render, focus survival — cannot be observed from rendered HTML, and a
/// reimplementation in the tests would be the same mistake as the hydration
/// shadow model that this crate carried until 0.2.0.
#[cfg(all(feature = "web", target_arch = "wasm32"))]
#[doc(hidden)]
pub fn build_dom_for_test(node: &Node) -> Option<WebNode> {
    create_dom_node(node)
}

/// Build a fresh DOM element for `el`, with its attributes, listeners, and
/// children.
///
/// Returns a comment node if `create_element` rejects the tag, so a bad tag
/// degrades to an inert placeholder rather than losing the whole subtree.
#[cfg(feature = "web")]
fn create_element_node(document: &web_sys::Document, el: &krab_core::Element) -> Option<WebNode> {
    let element = match document.create_element(&el.tag) {
        Ok(element) => element,
        Err(err) => {
            console::error_1(
                &format!(
                    "{{\"scope\":\"create_dom_node\",\"detail\":\"create_element failed\",\"tag\":\"{}\",\"error\":\"{:?}\"}}",
                    el.tag, err
                )
                .into(),
            );
            return Some(document.create_comment("krab-create-element-error").into());
        }
    };

    for attr in &el.attributes {
        if let Err(err) = element.set_attribute(&attr.name, &attr.value) {
            console::error_1(
                &format!(
                    "{{\"scope\":\"create_dom_node\",\"detail\":\"set_attribute failed\",\"attribute\":\"{}\",\"error\":\"{:?}\"}}",
                    attr.name, err
                )
                .into(),
            );
        }
    }

    // Same listener bookkeeping as the hydration path; this was a verbatim
    // duplicate of it before the two were unified.
    attach_element_events(&element, el, "create_dom_node");

    for child in &el.children {
        let Some(child_node) = create_dom_node(child) else {
            continue;
        };
        if let Err(err) = element.append_child(&child_node) {
            console::error_1(
                &format!(
                    "{{\"scope\":\"create_dom_node\",\"detail\":\"append child failed\",\"error\":\"{err:?}\"}}"
                )
                .into(),
            );
        }
    }

    Some(element.into())
}

#[cfg(feature = "web")]
fn create_dom_node(v_node: &Node) -> Option<WebNode> {
    let document = web_sys::window().and_then(|w| w.document())?;

    match v_node {
        Node::Element(el) => create_element_node(&document, el),
        Node::Text(text) => Some(document.create_text_node(text).into()),
        Node::Fragment(nodes) => {
            let frag = document.create_document_fragment();
            for node in nodes {
                if let Some(child_node) = create_dom_node(node) {
                    if let Err(err) = frag.append_child(&child_node) {
                        console::error_1(
                            &format!(
                                "{{\"scope\":\"create_dom_node\",\"detail\":\"append fragment child failed\",\"error\":\"{:?}\"}}",
                                err
                            )
                            .into(),
                        );
                    }
                }
            }
            Some(frag.into())
        }
        Node::Dynamic(f) => {
            // A `Dynamic` owns a *run* of sibling nodes, not one node.
            // `Node::Fragment` renders to several - a `<For>` renders one per
            // row - and `create_dom_node` returns a `DocumentFragment`, which
            // empties itself into the parent the moment it is appended. So the
            // run is tracked explicitly, terminated by a comment anchor that
            // stays in the DOM. The anchor is what makes an *empty* render
            // survivable, and it is what a *parent* region tracks when this
            // Dynamic is nested inside another (`create_tracked_child`) - the
            // returned fragment is a transient container, never a handle.
            let anchor: WebNode = document.create_comment("krab-dynamic").into();

            let cells = DynamicRegionCells {
                rendered: Rc::new(RefCell::new(Vec::new())),
                current_vnode: Rc::new(RefCell::new(None)),
                anchor: Rc::new(RefCell::new(Some(anchor.clone()))),
                empty_position: Rc::new(RefCell::new(None)),
                effect: Rc::new(RefCell::new(None)),
            };

            // A parent tracking this region by its anchor uses the registry to
            // remove the region's content and dispose its effect when the
            // region itself is removed. Registering before the effect exists is
            // fine: the registry holds a clone sharing the same effect slot.
            register_dynamic_region(&anchor, &cells);

            let f = f.clone();
            let first_run = Rc::new(Cell::new(true));
            let initial_inserts: Rc<RefCell<Vec<WebNode>>> = Rc::new(RefCell::new(Vec::new()));

            let effect_cells = cells.clone();
            let inserts_for_effect = initial_inserts.clone();

            let handle = create_effect_scoped(move || {
                let new_v_node = f();

                if first_run.get() {
                    first_run.set(false);
                    let mut inserts = Vec::new();
                    let mut tracked = Vec::new();
                    for (insert, track) in create_run_nodes(&new_v_node) {
                        inserts.push(insert);
                        tracked.push(track);
                    }
                    *inserts_for_effect.borrow_mut() = inserts;
                    *effect_cells.rendered.borrow_mut() = tracked;
                    *effect_cells.current_vnode.borrow_mut() = Some(new_v_node);
                    return;
                }

                update_dynamic_region(&effect_cells, new_v_node);
            });

            *cells.effect.borrow_mut() = Some(handle);
            own_region_effect_by_enclosing_effect(&cells);

            // Everything this `Dynamic` owns, in order, with the anchor last.
            let container = document.create_document_fragment();
            for node in initial_inserts.borrow().iter() {
                let _ = container.append_child(node);
            }
            let _ = container.append_child(&anchor);

            Some(container.into())
        }
    }
}

/// Build the DOM for one child vnode: the node to insert, and the node its
/// parent should *track*.
///
/// They differ only for [`Node::Dynamic`]. Its DOM is a `DocumentFragment`
/// that splices itself empty on insertion, so tracking the fragment leaves a
/// detached husk: later removals silently fail, the region's real nodes stay
/// behind, and the next update duplicates them. The stable handle is the
/// region's trailing anchor comment — the fragment's last child — which stays
/// in the DOM for the region's life and is registered in [`DYNAMIC_REGIONS`].
#[cfg(feature = "web")]
fn create_tracked_child(new_child: &Node) -> Option<(WebNode, WebNode)> {
    let created = create_dom_node(new_child)?;
    let tracked = if matches!(new_child, Node::Dynamic(_)) {
        created.last_child()?
    } else {
        created.clone()
    };
    Some((created, tracked))
}

/// Build the (insert, track) node pairs a vnode contributes to its parent.
///
/// A `Fragment` contributes its children rather than a node of its own, so this
/// returns a list. Everything else contributes exactly one pair.
#[cfg(feature = "web")]
fn create_run_nodes(node: &Node) -> Vec<(WebNode, WebNode)> {
    let mut flat: Vec<&Node> = Vec::new();
    flatten_children(std::slice::from_ref(node), &mut flat);
    flat.iter()
        .filter_map(|child| create_tracked_child(child))
        .collect()
}

/// Register a dynamic region under its anchor.
///
/// The map holds a *clone* of the cells, which shares every `Rc` with the live
/// region — including the effect slot, so a handle stored after registration is
/// still visible here.
#[cfg(feature = "web")]
fn register_dynamic_region(anchor: &WebNode, cells: &DynamicRegionCells) {
    let id = NEXT_DOM_ID.with(|id| {
        let v = id.get();
        id.set(v + 1);
        v
    });
    let _ = js_sys::Reflect::set(
        anchor.as_ref(),
        &JsValue::from_str("__krab_region"),
        &JsValue::from_f64(id as f64),
    );
    DYNAMIC_REGIONS.with(|regions| regions.borrow_mut().insert(id, cells.clone()));
}

/// Unregister the dynamic region anchored at `node`, returning its cells.
#[cfg(feature = "web")]
fn take_dynamic_region(node: &WebNode) -> Option<DynamicRegionCells> {
    let id = node_expando_id(node.as_ref(), "__krab_region")?;
    DYNAMIC_REGIONS.with(|regions| regions.borrow_mut().remove(&id))
}

/// Tear down a region's effect so it stops re-rendering nodes that are on their
/// way out.
///
/// The handle is *taken* before disposal, not borrowed: that both makes the
/// call idempotent and breaks the `Rc` cycle described on
/// [`DynamicRegionCells::effect`], so the effect's memory is actually
/// reclaimed. Disposal cascades — the effect owns the cleanups its own body
/// registered, which include the handles of any region nested inside it.
#[cfg(feature = "web")]
fn dispose_region_effect(cells: &DynamicRegionCells) {
    let handle = cells.effect.borrow_mut().take();
    if let Some(handle) = handle {
        handle.dispose();
    }
}

/// Hand this region's effect to the effect currently running, if there is one.
///
/// [`create_effect_scoped`] deliberately never adopts a parent — its lifetime
/// is the handle's — but a region built *during* another region's render must
/// still die when that render is superseded. Registering the disposal as an
/// `on_cleanup` on the enclosing effect restores exactly the ownership that
/// plain `create_effect` gave for free, without giving up the handle. Outside an
/// effect this is a no-op and the region lives until it is unmounted or its
/// anchor is removed.
#[cfg(feature = "web")]
fn own_region_effect_by_enclosing_effect(cells: &DynamicRegionCells) {
    let effect = cells.effect.clone();
    on_cleanup(move || {
        let handle = effect.borrow_mut().take();
        if let Some(handle) = handle {
            handle.dispose();
        }
    });
}

/// If `node` is a region anchor, tear the region down: dispose its effect and
/// remove its rendered content (its sibling nodes) from `el`, recursively
/// handling regions nested inside it. The anchor itself is left for the caller
/// to remove or replace.
///
/// Unlike [`release_dom_node_resources`], this removes DOM — the region's run
/// lives *beside* the anchor, so replacing the anchor alone would strand it.
#[cfg(feature = "web")]
fn remove_region_content(el: &Element, node: &WebNode) {
    let Some(region) = take_dynamic_region(node) else {
        return;
    };

    // Before the DOM goes, so a signal write during removal cannot make the
    // effect re-render against nodes that are half gone.
    dispose_region_effect(&region);

    for inner in region.rendered.borrow().iter() {
        remove_tracked_node(el, inner);
    }
}

/// Remove a tracked run entry: its region content and effect if it is a region
/// anchor, everything its subtree retains, and the node itself.
#[cfg(feature = "web")]
fn remove_tracked_node(el: &Element, node: &WebNode) {
    remove_region_content(el, node);
    release_dom_node_resources(node);
    let _ = el.remove_child(node);
}

#[wasm_bindgen(start)]
pub fn start() {
    console::log_1(&"Krab Client initialized".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use krab_core::{Attribute, Element, Node};

    #[test]
    fn boundary_state_from_factory_node_reads_decode_error_marker() {
        let fallback = Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![Attribute::new(
                "data-krab-boundary-state".to_string(),
                "decode-error".to_string(),
            )],
            children: vec![],
            events: vec![],
        });

        assert_eq!(
            boundary_state_from_factory_node(&fallback),
            Some("decode-error")
        );
    }

    #[test]
    fn classify_boundary_state_prefers_factory_error_and_patch_counts() {
        assert_eq!(
            classify_boundary_state(Some("decode-error"), 3),
            "decode-error"
        );
        assert_eq!(classify_boundary_state(None, 2), "patched");
        assert_eq!(classify_boundary_state(None, 0), "ok");
    }
}
