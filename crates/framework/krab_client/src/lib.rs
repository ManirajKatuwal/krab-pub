extern crate self as krab_client;

use krab_core::Node;
#[cfg(feature = "web")]
use std::cell::{Cell, RefCell};
#[cfg(feature = "web")]
use std::collections::{HashMap, VecDeque};
#[cfg(feature = "web")]
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(feature = "web")]
use std::rc::Rc;
use wasm_bindgen::prelude::*;
#[cfg(feature = "web")]
use wasm_bindgen::JsCast;
use web_sys::console;
#[cfg(feature = "web")]
use web_sys::{Element, HtmlElement, Node as WebNode, NodeList};

#[cfg(feature = "web")]
type EventClosure = Closure<dyn FnMut(web_sys::Event)>;
#[cfg(feature = "web")]
type EventClosureMap = std::collections::HashMap<u32, Vec<(String, EventClosure)>>;
#[cfg(feature = "web")]
type DynamicRegionMap = std::collections::HashMap<u32, Rc<RefCell<Vec<WebNode>>>>;

// Holds event-listener closures for the lifetime of the page or node so they are not
// dropped (which would invalidate the JS function pointer) but also not
// silently leaked via forget().
#[cfg(feature = "web")]
thread_local! {
    static EVENT_CLOSURES: RefCell<EventClosureMap> = RefCell::new(EventClosureMap::new());
    static NEXT_DOM_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
    /// The rendered run of every live [`Node::Dynamic`] region, keyed by the
    /// `__krab_region` expando on its anchor comment. A parent that tracks a
    /// nested region by its anchor uses this to remove the region's *content*
    /// too — the anchor alone cannot reach it.
    static DYNAMIC_REGIONS: RefCell<DynamicRegionMap> = RefCell::new(DynamicRegionMap::new());
}

pub use krab_core::signal::*;

pub mod components;
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

#[wasm_bindgen]
pub fn hydrate() {
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

        // Find all elements with data-island attribute
        let islands = match document.query_selector_all("[data-island]") {
            Ok(nodes) => nodes,
            Err(err) => {
                console::error_1(
                    &format!(
                        "{{\"scope\":\"hydrate\",\"detail\":\"query_selector_all failed\",\"error\":\"{:?}\"}}",
                        err
                    )
                    .into(),
                );
                return;
            }
        };

        for i in 0..islands.length() {
            let Some(element) = islands.item(i) else {
                console::warn_1(&format!("Missing island element at index {}", i).into());
                continue;
            };
            let Ok(html_element) = element.clone().dyn_into::<HtmlElement>() else {
                console::warn_1(
                    &format!("Island node at index {} is not an HtmlElement", i).into(),
                );
                continue;
            };

            let Some(name) = html_element.get_attribute("data-island") else {
                console::warn_1(
                    &format!("Island element at index {} missing data-island", i).into(),
                );
                continue;
            };
            let boundary_id_attr = html_element.get_attribute("data-krab-boundary-id");
            let boundary = HydrationBoundary {
                island: name.clone(),
                id: boundary_id_attr
                    .clone()
                    .unwrap_or_else(|| format!("{name}:legacy-{i}")),
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
            let _ = html_element.set_attribute("data-krab-boundary", &boundary.island);
            let _ = html_element.set_attribute("data-krab-boundary-id", &boundary.id);
            let _ = html_element.set_attribute("data-krab-boundary-mismatches", "0");
            let _ = html_element.set_attribute("data-krab-boundary-state", "hydrating");
            let props_json = html_element
                .get_attribute("data-props")
                .unwrap_or_else(|| "{}".to_string());

            // Find matching island definition
            let definition = inventory::iter::<IslandDefinition>
                .into_iter()
                .find(|def| def.name == name);

            if let Some(def) = definition {
                console::log_1(&format!("Hydrating island: {}", name).into());
                match catch_unwind(AssertUnwindSafe(|| (def.factory)(props_json))) {
                    Ok(node) => {
                        let node = krab_core::annotate_hydration_tree(node, &boundary.id);
                        let factory_state = boundary_state_from_factory_node(&node);
                        let stats = hydrate_node(element.clone(), &node, &boundary);
                        let mismatch_count = stats.mismatch_count();
                        let _ = html_element.set_attribute(
                            "data-krab-boundary-mismatches",
                            &mismatch_count.to_string(),
                        );
                        let _ = html_element.set_attribute(
                            "data-krab-boundary-state",
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
                    Err(_) => {
                        log_hydration_boundary_diagnostic(
                            "error",
                            "hydrate",
                            &boundary,
                            "factory_panic",
                            "factory panic captured",
                            None,
                        );
                        let _ = html_element.set_attribute("data-krab-boundary-state", "error");
                        html_element.set_inner_html(
                            "<div role=\"alert\">Hydration fallback rendered.</div>",
                        );
                    }
                }
            } else {
                let _ =
                    html_element.set_attribute("data-krab-boundary-state", "missing-definition");
                log_hydration_boundary_diagnostic(
                    "error",
                    "hydrate",
                    &boundary,
                    "missing_island_definition",
                    "no registered island definition found",
                    None,
                );
            }
        }
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
        remove_dom_node_closures(&extra_node);
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

/// Replace `old` with a freshly built node for `v_node`, releasing `old`'s
/// event closures first so they are not leaked.
#[cfg(feature = "web")]
fn replace_dom_node(parent: &WebNode, old: &WebNode, v_node: &Node, scope: &str) {
    let Some(new_node) = create_dom_node(v_node) else {
        return;
    };

    remove_dom_node_closures(old);
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
    let Ok(id_val) = js_sys::Reflect::get(real_el.as_ref(), &JsValue::from_str("__krab_id")) else {
        return;
    };
    let Some(id) = id_val.as_f64() else {
        return;
    };
    let Some(pairs) = EVENT_CLOSURES.with(|v| v.borrow_mut().remove(&(id as u32))) else {
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
#[cfg(feature = "web")]
fn attach_element_events(real_el: &Element, v_el: &krab_core::Element, scope: &str) {
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
    let real_node_opt = node_list.item(index);

    match v_node {
        Node::Element(v_el) => {
            hydrate_element(parent, node_list, index, v_node, v_el, boundary, path)
        }
        Node::Text(text) => hydrate_text(parent, real_node_opt, v_node, text, boundary, path),
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
            };

            // The initial hydration runs *inside* the effect's first run, so a
            // Dynamic nested in the SSR content creates its own effect while
            // this one is current - making it an owned child that is disposed
            // when this region re-renders. Hydrating first and creating the
            // effect afterwards left every nested region's effect in
            // `ROOT_EFFECTS`: immortal, subscribed, and re-rendering detached
            // DOM for the life of the page.
            let stats_cell: Rc<RefCell<HydrationStats>> =
                Rc::new(RefCell::new(HydrationStats::default()));

            let f = f.clone();
            let first_run = Rc::new(Cell::new(true));
            let effect_cells = DynamicRegionCells {
                rendered: cells.rendered.clone(),
                current_vnode: cells.current_vnode.clone(),
                anchor: cells.anchor.clone(),
                empty_position: cells.empty_position.clone(),
            };
            let parent = parent.clone();
            let node_list = node_list.clone();
            let boundary = boundary.clone();
            let path = path.to_string();
            let stats_for_effect = stats_cell.clone();

            create_effect(move || {
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
                    *stats_for_effect.borrow_mut() = stats;
                    return;
                }

                update_dynamic_region(&effect_cells, new_v_node);
            });

            // `create_effect` ran synchronously, so the stats are populated.
            let stats = *stats_cell.borrow();
            let _ = cells;
            stats
        }
    }
}

#[cfg(feature = "web")]
fn remove_dom_node_closures(node: &WebNode) {
    if let Ok(id_val) = js_sys::Reflect::get(node.as_ref(), &JsValue::from_str("__krab_id")) {
        if let Some(id_f64) = id_val.as_f64() {
            EVENT_CLOSURES.with(|v| v.borrow_mut().remove(&(id_f64 as u32)));
        }
    }
    let child_nodes = node.child_nodes();
    for i in 0..child_nodes.length() {
        if let Some(child) = child_nodes.item(i) {
            remove_dom_node_closures(&child);
        }
    }
}

/// The state one [`Node::Dynamic`] region shares between its effect runs.
///
/// One definition serves both the hydration and the client-mount paths — the
/// two previously carried verbatim copies of the update logic, and the copies
/// had already diverged (the hydration copy could not create its anchor for an
/// initially-empty run, leaving the region permanently dead).
#[cfg(feature = "web")]
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

                register_dynamic_region(&created, cells.rendered.clone());
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
                detach_element_events(el);
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
                    remove_dom_node_closures(&node);
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
            };

            // A parent tracking this region by its anchor uses the registry to
            // remove the region's content when the region itself is removed.
            register_dynamic_region(&anchor, cells.rendered.clone());

            let f = f.clone();
            let first_run = Rc::new(Cell::new(true));
            let initial_inserts: Rc<RefCell<Vec<WebNode>>> = Rc::new(RefCell::new(Vec::new()));

            let effect_cells = DynamicRegionCells {
                rendered: cells.rendered.clone(),
                current_vnode: cells.current_vnode.clone(),
                anchor: cells.anchor.clone(),
                empty_position: cells.empty_position.clone(),
            };
            let inserts_for_effect = initial_inserts.clone();

            create_effect(move || {
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

/// Register a dynamic region's rendered run under its anchor.
#[cfg(feature = "web")]
fn register_dynamic_region(anchor: &WebNode, rendered: Rc<RefCell<Vec<WebNode>>>) {
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
    DYNAMIC_REGIONS.with(|regions| regions.borrow_mut().insert(id, rendered));
}

/// If `node` is a region anchor, remove the region's rendered content (its
/// sibling nodes) from `el`, recursively handling regions nested inside it.
/// The anchor itself is left for the caller to remove or replace.
#[cfg(feature = "web")]
fn remove_region_content(el: &Element, node: &WebNode) {
    let Ok(id_val) = js_sys::Reflect::get(node.as_ref(), &JsValue::from_str("__krab_region"))
    else {
        return;
    };
    let Some(id) = id_val.as_f64() else {
        return;
    };
    let Some(rendered) = DYNAMIC_REGIONS.with(|regions| regions.borrow_mut().remove(&(id as u32)))
    else {
        return;
    };
    for inner in rendered.borrow().iter() {
        remove_tracked_node(el, inner);
    }
}

/// Remove a tracked run entry: its listeners, its region content if it is a
/// region anchor, and the node itself.
#[cfg(feature = "web")]
fn remove_tracked_node(el: &Element, node: &WebNode) {
    remove_region_content(el, node);
    remove_dom_node_closures(node);
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
