//! The hydration walk: finding island boundaries, rebuilding each island's
//! virtual tree from its registered factory, adopting the server-rendered DOM
//! against it, and stamping the boundary with the outcome.

use wasm_bindgen::prelude::*;

// Without `web` this module is only the no-op `hydrate()` export and the pure
// boundary-classification helpers the host tests cover, so most imports are
// gated to match.
#[cfg(any(feature = "web", test))]
use krab_core::Node;
#[cfg(any(feature = "web", feature = "debug"))]
use web_sys::console;

#[cfg(feature = "web")]
use crate::reconcile::{
    append_dom_node, replace_dom_node, update_dynamic_region, DynamicRegionCells,
};
#[cfg(feature = "web")]
use crate::resources::{
    attach_element_events, bind_dynamic_attributes, own_region_effect_by_enclosing_effect,
    release_dom_node_resources,
};
#[cfg(feature = "web")]
use crate::{ComponentFactory, IslandDefinition};
#[cfg(feature = "web")]
use krab_core::signal::create_effect_scoped;
#[cfg(feature = "web")]
use std::cell::{Cell, RefCell};
#[cfg(feature = "web")]
use std::collections::HashMap;
#[cfg(feature = "web")]
use std::rc::Rc;
#[cfg(feature = "web")]
use wasm_bindgen::JsCast;
#[cfg(feature = "web")]
use web_sys::{Element, Node as WebNode, NodeList};

// Only the non-wasm32 island error boundary uses these; on wasm32 the target is
// `panic = "abort"` and the boundary is deliberately absent. See `hydrate_island`.
#[cfg(all(feature = "web", not(target_arch = "wasm32")))]
use std::panic::{catch_unwind, AssertUnwindSafe};

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
pub(crate) fn node_hydration_id(node: &Node) -> Option<&str> {
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

/// The markup a boundary is left holding when its hydration fails outright.
///
/// `role="alert"` so the loss of interactivity is announced rather than silent.
#[cfg(feature = "web")]
const FALLBACK_HTML: &str = "<div role=\"alert\">Hydration fallback rendered.</div>";

/// Hydrate one island boundary against the DOM element the server rendered it
/// into.
///
/// `ordinal` is only used to name a boundary whose markup predates
/// `data-krab-boundary-id`.
///
/// Callers go through [`HydrationPass::hydrate`], never here directly: on wasm32
/// that is what puts this function behind the panic-isolation trampoline.
#[cfg(feature = "web")]
fn hydrate_boundary(
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

    // A panicking island factory cannot be caught *here* on wasm32.
    // `wasm32-unknown-unknown` is `panic = "abort"` (`rustc --print cfg`
    // confirms it), so `catch_unwind` would never return `Err`: the panic hook
    // runs, then `unreachable` traps. A `catch_unwind` here once implied a
    // recovery that never happened, and the trap took every later island in
    // the pass down with it.
    //
    // The panic is contained one frame further out instead. On wasm32 this
    // whole function runs inside `isolation::call_isolated`, whose JS
    // `try`/`catch` observes the trap; `HydrationPass::hydrate` then stamps this
    // boundary `error`, renders the fallback, and moves on to the next island.
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
        element.set_inner_html(FALLBACK_HTML);
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
///
/// Each island is isolated from the others: one whose factory panics is stamped
/// `data-krab-boundary-state="error"` and given the fallback markup, and the
/// islands after it still hydrate. See [`hydrate`] for what that costs.
///
/// Exported to JavaScript as `hydrate_within(element)`.
#[cfg(feature = "web")]
#[wasm_bindgen(js_name = hydrate_within)]
pub fn hydrate_within(root: &web_sys::Element) {
    let mut pass = HydrationPass::new();
    hydrate_subtree(&mut pass, root);
}

/// Hydrate one island element, and nothing else.
///
/// Returns the `data-krab-boundary-state` the boundary ends in — `ok`,
/// `patched`, `decode-error`, `missing-definition`, or `error` when the island
/// panicked — or, for a boundary an earlier pass already claimed, the state it
/// already had. Returns `None` (`undefined` in JavaScript) when `element` does
/// not carry `data-island`. Islands *nested* inside `element` are not touched;
/// use [`hydrate_within`] for a subtree.
///
/// Isolated exactly like [`hydrate`]: a panic is contained to this island and
/// never surfaces as a thrown `RuntimeError`.
///
/// Exported to JavaScript as `hydrate_island(element)`. It builds the island
/// registry lookup on every call, so a page hydrating many islands at once
/// should prefer [`hydrate_within_selector`], which builds it once.
#[cfg(feature = "web")]
#[wasm_bindgen(js_name = hydrate_island)]
pub fn hydrate_island(element: &web_sys::Element) -> Option<String> {
    if !element.has_attribute("data-island") {
        return None;
    }
    let mut pass = HydrationPass::new();
    pass.hydrate(element);
    element.get_attribute(BOUNDARY_STATE_ATTR)
}

/// Hydrate every element in the document matching `selector` — each one itself
/// if it is an island, and every island inside it.
///
/// This is the scoped entry point for pages that hydrate in stages: critical
/// islands first, the rest when the browser is idle.
///
/// ```js
/// import init, { hydrate_within_selector } from '/pkg/app.js';
/// await init();
/// hydrate_within_selector('[data-island="Counter"]');
/// requestIdleCallback(() => hydrate_within_selector('[data-island]'));
/// ```
///
/// The second call is safe to make over islands the first already hydrated:
/// they are skipped, not bound twice. Returns how many boundaries **failed**
/// during this call — ended in `error` because they panicked — so a caller can
/// degrade visibly without scanning the DOM. An invalid selector is reported on
/// the console and hydrates nothing.
///
/// Exported to JavaScript as `hydrate_within_selector(selector)`.
#[cfg(feature = "web")]
#[wasm_bindgen(js_name = hydrate_within_selector)]
pub fn hydrate_within_selector(selector: &str) -> u32 {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        console::error_1(&"{\"scope\":\"hydrate\",\"detail\":\"missing document\"}".into());
        return 0;
    };
    let matches = match document.query_selector_all(selector) {
        Ok(matches) => matches,
        Err(err) => {
            log_hydration_diagnostic(
                "hydrate",
                "",
                &format!("invalid selector {selector:?}: {err:?}"),
            );
            return 0;
        }
    };

    let mut pass = HydrationPass::new();
    for index in 0..matches.length() {
        let Some(element) = matches
            .item(index)
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            continue;
        };
        // A match inside an earlier match whose island failed, or whose DOM
        // the walk replaced, is detached by now.
        if !element.is_connected() {
            continue;
        }
        hydrate_subtree(&mut pass, &element);
    }
    pass.failed
}

/// Hydrate `root` if it is an island, then every island beneath it.
#[cfg(feature = "web")]
fn hydrate_subtree(pass: &mut HydrationPass, root: &Element) {
    if root.has_attribute("data-island") {
        pass.hydrate(root);
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
        // The list is a snapshot taken before any island ran. An island nested
        // inside one that failed — its content replaced by the fallback — or
        // inside a node the hydration walk replaced is no longer under `root`,
        // and hydrating it would bind closures to DOM nobody can see.
        if !root.contains(Some(element.as_ref())) {
            continue;
        }
        pass.hydrate(&element);
    }
}

/// One hydration pass: what every boundary in it shares.
///
/// The factory map is built once per pass, not once per boundary. On wasm32 the
/// pass also owns the single closure every island is hydrated through, so the
/// panic-isolation trampoline costs one JS crossing per island and no
/// allocation per island.
#[cfg(feature = "web")]
struct HydrationPass {
    next_ordinal: u32,
    /// Boundaries that ended in `error` during this pass.
    failed: u32,
    #[cfg(not(target_arch = "wasm32"))]
    factories: HashMap<&'static str, ComponentFactory>,
    /// The ordinal of the boundary the closure is about to hydrate. The
    /// closure takes only the element, so the ordinal travels beside it.
    #[cfg(target_arch = "wasm32")]
    ordinal: Rc<Cell<u32>>,
    #[cfg(target_arch = "wasm32")]
    entry: Closure<dyn FnMut(Element)>,
}

#[cfg(feature = "web")]
impl HydrationPass {
    fn new() -> Self {
        let factories = island_factories();

        #[cfg(target_arch = "wasm32")]
        let ordinal = Rc::new(Cell::new(0u32));
        #[cfg(target_arch = "wasm32")]
        let entry = {
            let ordinal = ordinal.clone();
            Closure::<dyn FnMut(Element)>::new(move |element: Element| {
                hydrate_boundary(&element, ordinal.get(), &factories);
            })
        };

        Self {
            next_ordinal: 0,
            failed: 0,
            #[cfg(not(target_arch = "wasm32"))]
            factories,
            #[cfg(target_arch = "wasm32")]
            ordinal,
            #[cfg(target_arch = "wasm32")]
            entry,
        }
    }

    /// Hydrate one boundary, containing a failure to that boundary.
    fn hydrate(&mut self, element: &Element) {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let already_claimed = boundary_is_hydrated(element);

        #[cfg(target_arch = "wasm32")]
        {
            self.ordinal.set(ordinal);
            // Taken before the island runs: a trap discards its frames without
            // running a single drop guard, so the reactive runtime's current
            // owner, subscriber and batch depth would otherwise stay as the
            // dead island left them — the next island's scope parented to the
            // dead one (inheriting its contexts), and a stuck batch deferring
            // every later write on the page.
            let snapshot = krab_core::signal::snapshot_reactive_state();
            if let Err(message) = crate::isolation::call_isolated(&self.entry, element) {
                restore_after_trap(snapshot, element, ordinal);
                contain_failed_boundary(element, ordinal, &message);
            }
        }

        // Off wasm32 unwinding is real, and `hydrate_boundary`'s own
        // `catch_unwind` already contains a panicking factory.
        #[cfg(not(target_arch = "wasm32"))]
        hydrate_boundary(element, ordinal, &self.factories);

        if !already_claimed
            && element.get_attribute(BOUNDARY_STATE_ATTR).as_deref() == Some("error")
        {
            self.failed += 1;
        }
    }
}

/// Put a boundary whose hydration trapped into its terminal `error` state.
///
/// This runs in the frame *below* the trap, after every frame that was
/// hydrating the boundary has been discarded mid-flight. Listeners it bound
/// before the panic are still registered, and the DOM may be half-patched.
#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn contain_failed_boundary(element: &Element, ordinal: u32, message: &str) {
    // Stamped before anything that touches runtime state the trap may have
    // left inconsistent: if one of those steps traps too, the boundary must
    // still read as failed rather than as `hydrating` forever.
    let _ = element.set_attribute(BOUNDARY_STATE_ATTR, "error");

    let island = element.get_attribute("data-island").unwrap_or_default();
    let boundary = HydrationBoundary {
        id: element
            .get_attribute("data-krab-boundary-id")
            .unwrap_or_else(|| format!("{island}:legacy-{ordinal}")),
        island,
    };
    log_hydration_boundary_diagnostic("error", "hydrate", &boundary, "island_panic", message, None);

    // The old content is detached before its resources are released, so the
    // fallback is on screen even if the release trips over state the trap
    // left behind (a `RefCell` still borrowed, say).
    let stale = element.child_nodes();
    let stale: Vec<WebNode> = (0..stale.length())
        .filter_map(|index| stale.item(index))
        .collect();
    element.set_inner_html(FALLBACK_HTML);

    // The release disposes region effects, which runs user `on_cleanup`
    // callbacks, in the post-trap state. A second panic there used to escape
    // `hydrate_within` itself and strand every later island, so it runs
    // isolated too, and a trap in it is logged and survived.
    let snapshot = krab_core::signal::snapshot_reactive_state();
    let release = Closure::<dyn FnMut(Element)>::new(move |_: Element| {
        for node in &stale {
            release_dom_node_resources(node);
        }
    });
    if let Err(message) = crate::isolation::call_isolated(&release, element) {
        restore_after_trap(snapshot, element, ordinal);
        log_hydration_boundary_diagnostic(
            "error",
            "hydrate",
            &boundary,
            "island_cleanup_panic",
            &message,
            None,
        );
    }
}

/// Put the reactive runtime back as it was before an isolated call trapped.
/// A cell the trap left borrowed cannot be restored; that is reported, since
/// the next use of it will panic (and be contained, one island at a time).
#[cfg(all(feature = "web", target_arch = "wasm32"))]
fn restore_after_trap(
    snapshot: krab_core::signal::ReactiveSnapshot,
    element: &Element,
    ordinal: u32,
) {
    if krab_core::signal::restore_reactive_state(snapshot) {
        return;
    }
    let island = element.get_attribute("data-island").unwrap_or_default();
    let boundary = HydrationBoundary {
        id: element
            .get_attribute("data-krab-boundary-id")
            .unwrap_or_else(|| format!("{island}:legacy-{ordinal}")),
        island,
    };
    log_hydration_boundary_diagnostic(
        "error",
        "hydrate",
        &boundary,
        "reactive_state_unrecoverable",
        "a trap left the reactive runtime's state borrowed; it could not be restored",
        None,
    );
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
/// Exported to JavaScript as `unmount(element)`, for pages that swap DOM
/// themselves.
///
/// # Limitation
///
/// A dynamic region that has not yet re-rendered has no anchor comment in the
/// DOM and so cannot be found by a subtree walk; its effect is released when
/// its enclosing region re-renders or is removed. Regions that have rendered at
/// least once — the ones that own DOM — are always released here.
#[cfg(feature = "web")]
#[wasm_bindgen(js_name = unmount)]
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

/// Hydrate every island in the document.
///
/// Delegates to [`hydrate_within`] over the document element, so the two share
/// one implementation and one idempotence rule: a boundary whose
/// `data-krab-boundary-state` has moved off `ssr` is left alone.
///
/// # Panic isolation
///
/// Islands are isolated from one another. A panic in one island's factory —
/// or anywhere in its hydration — marks that boundary
/// `data-krab-boundary-state="error"`, replaces its content with a
/// `role="alert"` fallback, logs an `island_panic` diagnostic carrying the
/// panic message, and hydration continues with the next island. `hydrate()`
/// itself never throws for it.
///
/// On wasm32, where panics abort, this works by hydrating each island through
/// a JavaScript `try`/`catch` shipped inside the bundle: one extra JS crossing
/// per island, and nothing on a page with no panics. The recovery is
/// best-effort — the frames that panicked are abandoned mid-flight, so their
/// allocations leak and a `RefCell` they held borrowed stays borrowed. The
/// reactive runtime's current owner, subscriber and batch depth are restored
/// from a snapshot taken before the island ran, so the next island neither
/// inherits the dead one's contexts nor finds signal writes deferred forever.
/// That is the price of the fallback path only; before isolation existed the
/// same panic left every later island on the page unhydrated, with no
/// diagnostic.
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

        install_suspense_listener(&document);
        hydrate_within(&root);
    }
}

#[cfg(feature = "web")]
type DocumentListener = Closure<dyn FnMut(web_sys::Event)>;

/// What a swap-event listener does to each element it names.
#[cfg(feature = "web")]
type ElementHook = fn(&Element);

#[cfg(feature = "web")]
thread_local! {
    /// The document listeners for streamed `<Suspense>` content, installed once
    /// per page by [`hydrate`]. Held rather than `forget()`ed, like every other
    /// closure this crate hands to the DOM.
    static SUSPENSE_LISTENER: RefCell<Vec<DocumentListener>> = const { RefCell::new(Vec::new()) };
}

/// Listen for the two events the streaming swap runtime
/// (`krab_core::render_stream::STREAM_SWAP_SCRIPT`, ADR 0017) dispatches:
///
/// - `krab:suspense-resolving`, just before it removes a boundary's fallback:
///   [`unmount`] each outgoing element, so an island hydrated in the fallback
///   does not keep its closures and effects alive after its DOM is gone.
/// - `krab:suspense-resolved`, after it moves the streamed content in:
///   hydrate the islands in that content.
///
/// Content that arrived before the bundle loaded needs nothing from this: it is
/// already in the document when [`hydrate`] walks it. The listeners cover
/// content that streams in *after* hydration ran.
#[cfg(feature = "web")]
fn install_suspense_listener(document: &web_sys::Document) {
    SUSPENSE_LISTENER.with(|slot| {
        if !slot.borrow().is_empty() {
            return;
        }
        let listeners: [(&str, ElementHook); 2] = [
            ("krab:suspense-resolving", unmount),
            ("krab:suspense-resolved", hydrate_within),
        ];
        for (name, apply) in listeners {
            let closure =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                    for element in suspense_event_elements(&event) {
                        if element.is_connected() {
                            apply(&element);
                        }
                    }
                });
            match document.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref())
            {
                Ok(()) => slot.borrow_mut().push(closure),
                Err(err) => log_hydration_diagnostic(
                    "hydrate",
                    "",
                    &format!("could not listen for {name}: {err:?}"),
                ),
            }
        }
    });
}

/// The elements in a swap event's `detail.nodes`; text and comment nodes are
/// skipped, since neither can hold an island.
#[cfg(feature = "web")]
fn suspense_event_elements(event: &web_sys::Event) -> Vec<Element> {
    let nodes = js_sys::Reflect::get(event, &JsValue::from_str("detail"))
        .and_then(|detail| js_sys::Reflect::get(&detail, &JsValue::from_str("nodes")))
        .unwrap_or(JsValue::UNDEFINED);
    if !js_sys::Array::is_array(&nodes) {
        return Vec::new();
    }
    js_sys::Array::from(&nodes)
        .iter()
        .filter_map(|node| node.dyn_into::<Element>().ok())
        .collect()
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
        // Adopting: the first run writes only where the server markup
        // disagrees with the source, so a correct attribute is not rewritten.
        bind_dynamic_attributes(real_el, v_el, true);
        // `<script>` and `<style>` hold raw text. The server emits it with
        // breakout sequences rewritten (`</script` as `<\/script`), so the DOM
        // text legitimately differs from the vnode text, and "patching" it
        // would change nothing a browser acts on — a script has already run,
        // and its text node is not re-executed. The element is adopted as-is.
        if v_el.tag.eq_ignore_ascii_case("script") || v_el.tag.eq_ignore_ascii_case("style") {
            let mut stats = HydrationStats::default().with_consumed(1);
            stats.merge(alignment_stats);
            return stats;
        }
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

/// Hydrate one `Node::Comment` — in practice a `<Suspense>` boundary marker —
/// against the DOM child at `index`.
///
/// A comment is adopted without comparing its text. The markers carry boundary
/// ids from a per-process counter, which the server and the browser advance
/// independently, so the text legitimately differs; and a comment's text
/// changes nothing on screen. What matters is that it occupies its slot, so
/// the siblings after it line up.
#[cfg(feature = "web")]
fn hydrate_comment(
    parent: &WebNode,
    real_node_opt: Option<WebNode>,
    text: &str,
    boundary: &HydrationBoundary,
    path: &str,
) -> HydrationStats {
    /// `Node.COMMENT_NODE`
    const COMMENT_NODE: u16 = 8;

    if real_node_opt
        .as_ref()
        .is_some_and(|node| node.node_type() == COMMENT_NODE)
    {
        return HydrationStats {
            consumed: 1,
            ..HydrationStats::default()
        };
    }

    // Missing, or something else sits in its slot. Insert rather than
    // replace: a comment has no content to lose, but the node it would
    // replace does — it is left for the next sibling's vnode to claim.
    log_hydration_boundary_diagnostic(
        "warn",
        "hydrate_recursive",
        boundary,
        "missing_comment_node",
        "expected a comment marker; inserting one",
        Some(path),
    );
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return HydrationStats::default();
    };
    let comment = document.create_comment(text);
    if let Err(err) = parent.insert_before(&comment, real_node_opt.as_ref()) {
        console::error_1(
            &format!(
                "{{\"scope\":\"hydrate_recursive\",\"detail\":\"insert comment failed\",\"error\":\"{err:?}\"}}"
            )
            .into(),
        );
        return HydrationStats::default();
    }
    HydrationStats {
        consumed: 1,
        appends: 1,
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
        Node::Comment(text) => hydrate_comment(parent, node_list.item(index), text, boundary, path),
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
