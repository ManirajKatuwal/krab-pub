extern crate self as krab_client;

use krab_core::Node;
#[cfg(feature = "web")]
use std::cell::{Cell, RefCell};
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
type EventClosureMap = std::collections::HashMap<u32, Vec<EventClosure>>;

// Holds event-listener closures for the lifetime of the page or node so they are not
// dropped (which would invalidate the JS function pointer) but also not
// silently leaked via forget().
#[cfg(feature = "web")]
thread_local! {
    static EVENT_CLOSURES: RefCell<EventClosureMap> = RefCell::new(EventClosureMap::new());
    static NEXT_DOM_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

pub use krab_core::signal::*;

pub mod components;
pub use components::*;

pub mod router;

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

#[cfg(any(feature = "web", test))]
fn node_hydration_id(node: &Node) -> Option<&str> {
    node_attribute_value(node, krab_core::HYDRATION_NODE_ID_ATTR)
}

#[cfg(test)]
fn element_hydration_id(element: &krab_core::Element) -> Option<&str> {
    element
        .attributes
        .iter()
        .find(|attr| attr.name == krab_core::HYDRATION_NODE_ID_ATTR)
        .map(|attr| attr.value.as_str())
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
    }

    fn mismatch_count(self) -> u32 {
        self.replacements + self.appends + self.removals + self.reorders + self.text_patches
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
            return (Some(current_node), HydrationStats::default());
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
            let mut alignment_stats = HydrationStats::default();
            let aligned_node_opt = if let Some(expected_id) = node_hydration_id(v_node) {
                let (aligned_node_opt, stats) = realign_node_by_hydration_id(
                    parent,
                    node_list,
                    index,
                    expected_id,
                    boundary,
                    path,
                );
                alignment_stats = stats;
                aligned_node_opt
            } else {
                real_node_opt
            };

            if let Some(real_node) = aligned_node_opt {
                let mut match_found = false;
                if let Some(real_el) = real_node.dyn_ref::<Element>() {
                    if real_el.tag_name().to_lowercase() == v_el.tag.to_lowercase() {
                        match_found = true;
                        // Attach events
                        #[cfg(feature = "web")]
                        {
                            let mut node_closures = Vec::new();
                            for event in &v_el.events {
                                let name = event.name.clone();
                                let callback = event.callback.clone();

                                let closure = Closure::wrap(Box::new(move |e: web_sys::Event| {
                                    callback(e);
                                })
                                    as Box<dyn FnMut(_)>);

                                if let Err(err) = real_el.add_event_listener_with_callback(
                                    &name,
                                    closure.as_ref().unchecked_ref(),
                                ) {
                                    console::error_1(
                                        &format!(
                                            "{{\"scope\":\"hydrate_recursive\",\"detail\":\"failed to attach event listener\",\"event\":\"{}\",\"error\":\"{:?}\"}}",
                                            name, err
                                        )
                                        .into(),
                                    );
                                }
                                node_closures.push(closure);
                            }
                            if !node_closures.is_empty() {
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
                        }
                    }
                }

                if match_found {
                    let mut stats = hydrate_children(&real_node, &v_el.children, boundary, path)
                        .with_consumed(1);
                    stats.merge(alignment_stats);
                    stats
                } else {
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
                    if let Some(new_node) = create_dom_node(v_node) {
                        remove_dom_node_closures(&real_node);
                        if let Err(err) = parent.replace_child(&new_node, &real_node) {
                            console::error_1(
                                &format!(
                                    "{{\"scope\":\"hydrate_recursive\",\"detail\":\"replace_child failed\",\"error\":\"{:?}\"}}",
                                    err
                                )
                                .into(),
                            );
                        }
                    }
                    let mut stats = HydrationStats {
                        consumed: 1,
                        replacements: 1,
                        ..HydrationStats::default()
                    };
                    stats.merge(alignment_stats);
                    stats
                }
            } else {
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
                if let Some(new_node) = create_dom_node(v_node) {
                    if let Err(err) = parent.append_child(&new_node) {
                        console::error_1(
                            &format!(
                                "{{\"scope\":\"hydrate_recursive\",\"detail\":\"append_child failed\",\"error\":\"{:?}\"}}",
                                err
                            )
                            .into(),
                        );
                    }
                }
                let mut stats = HydrationStats {
                    consumed: 1,
                    appends: 1,
                    ..HydrationStats::default()
                };
                stats.merge(alignment_stats);
                stats
            }
        }
        Node::Text(text) => {
            if let Some(real_node) = real_node_opt {
                if real_node.node_type() == 3 {
                    // Text node
                    if real_node.text_content().unwrap_or_default() != *text {
                        log_hydration_boundary_diagnostic(
                            "warn",
                            "hydrate_recursive",
                            boundary,
                            "text_content_mismatch",
                            &format!(
                                "expected text {:?} but found {:?}; patching text node",
                                text,
                                real_node.text_content().unwrap_or_default()
                            ),
                            Some(path),
                        );
                        real_node.set_text_content(Some(text));
                        HydrationStats {
                            consumed: 1,
                            text_patches: 1,
                            ..HydrationStats::default()
                        }
                    } else {
                        HydrationStats {
                            consumed: 1,
                            ..HydrationStats::default()
                        }
                    }
                } else {
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
                    if let Some(new_node) = create_dom_node(v_node) {
                        remove_dom_node_closures(&real_node);
                        if let Err(err) = parent.replace_child(&new_node, &real_node) {
                            console::error_1(
                                 &format!(
                                     "{{\"scope\":\"hydrate_recursive\",\"detail\":\"replace text node failed\",\"error\":\"{:?}\"}}",
                                     err
                                 )
                                 .into(),
                             );
                        }
                    }
                    HydrationStats {
                        consumed: 1,
                        replacements: 1,
                        ..HydrationStats::default()
                    }
                }
            } else {
                log_hydration_boundary_diagnostic(
                    "warn",
                    "hydrate_recursive",
                    boundary,
                    "missing_dom_node",
                    &format!(
                        "expected text {:?} but DOM child was missing; appending",
                        text
                    ),
                    Some(path),
                );
                if let Some(new_node) = create_dom_node(v_node) {
                    if let Err(err) = parent.append_child(&new_node) {
                        console::error_1(
                             &format!(
                                 "{{\"scope\":\"hydrate_recursive\",\"detail\":\"append text node failed\",\"error\":\"{:?}\"}}",
                                 err
                             )
                             .into(),
                         );
                    }
                }
                HydrationStats {
                    consumed: 1,
                    appends: 1,
                    ..HydrationStats::default()
                }
            }
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
            // Run the function once to get the initial structure (should match SSR)
            // Note: This run is NOT tracked by effect yet.
            let initial_v_node = f();

            // Hydrate the initial dynamic output and retain the current root node reference
            // so later reactive updates can replace it.

            let stats =
                hydrate_recursive(parent, node_list, index, &initial_v_node, boundary, path);

            let current_dom_node = Rc::new(RefCell::new(node_list.item(index)));
            let current_vnode = Rc::new(RefCell::new(Some(initial_v_node)));

            // Set up effect for future updates
            let f = f.clone();
            let first_run = Rc::new(Cell::new(true));
            let current_node_ref = current_dom_node.clone();

            create_effect(move || {
                let new_v_node = f();

                if first_run.get() {
                    first_run.set(false);
                    return;
                }

                let old_node = current_node_ref.borrow();
                let old_v = current_vnode.borrow();

                if let Some(old) = old_node.as_ref() {
                    if let Some(parent) = old.parent_node() {
                        let mut patched = false;
                        if let Some(old_vnode) = old_v.as_ref() {
                            if let Some(patched_node) =
                                patch_dom(&parent, old, old_vnode, &new_v_node)
                            {
                                *current_node_ref.borrow_mut() = Some(patched_node);
                                patched = true;
                            }
                        }

                        if !patched {
                            if let Some(new_dom_node) = create_dom_node(&new_v_node) {
                                remove_dom_node_closures(old);
                                if let Err(err) = parent.replace_child(&new_dom_node, old) {
                                    console::error_1(
                                        &format!(
                                            "{{\"scope\":\"dynamic\",\"detail\":\"replace_child failed\",\"error\":\"{:?}\"}}",
                                            err
                                        )
                                        .into(),
                                    );
                                    return;
                                }
                                *current_node_ref.borrow_mut() = Some(new_dom_node);
                            }
                        }
                    }
                }

                drop(old_v);
                *current_vnode.borrow_mut() = Some(new_v_node);
            });

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

            // Children - simple 1:1 patching
            if old_el.children.len() == new_el.children.len() {
                let child_nodes = el.child_nodes();
                let mut dom_index = 0;
                for i in 0..old_el.children.len() {
                    let old_child = &old_el.children[i];
                    let new_child = &new_el.children[i];
                    if let Some(child_dom) = child_nodes.item(dom_index) {
                        patch_dom(el, &child_dom, old_child, new_child)?;

                        fn count_dom_nodes(n: &Node) -> u32 {
                            match n {
                                Node::Fragment(c) => c.iter().map(count_dom_nodes).sum(),
                                _ => 1,
                            }
                        }
                        dom_index += count_dom_nodes(new_child);
                    } else {
                        return None;
                    }
                }
                return Some(real_node.clone());
            }
            None
        }
        _ => None,
    }
}

#[cfg(feature = "web")]
fn create_dom_node(v_node: &Node) -> Option<WebNode> {
    let document = web_sys::window().and_then(|w| w.document())?;

    match v_node {
        Node::Element(el) => {
            let element = match document.create_element(&el.tag) {
                Ok(elm) => elm,
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

            // Attach events for newly created nodes
            #[cfg(feature = "web")]
            {
                let mut node_closures = Vec::new();
                for event in &el.events {
                    let name = event.name.clone();
                    let callback = event.callback.clone();
                    let closure = Closure::wrap(Box::new(move |e: web_sys::Event| {
                        callback(e);
                    }) as Box<dyn FnMut(_)>);
                    if let Err(err) = element
                        .add_event_listener_with_callback(&name, closure.as_ref().unchecked_ref())
                    {
                        console::error_1(
                            &format!(
                                "{{\"scope\":\"create_dom_node\",\"detail\":\"add_event_listener failed\",\"event\":\"{}\",\"error\":\"{:?}\"}}",
                                name, err
                            )
                            .into(),
                         );
                    }
                    node_closures.push(closure);
                }
                if !node_closures.is_empty() {
                    let id = NEXT_DOM_ID.with(|id| {
                        let v = id.get();
                        id.set(v + 1);
                        v
                    });
                    let _ = js_sys::Reflect::set(
                        element.as_ref(),
                        &JsValue::from_str("__krab_id"),
                        &JsValue::from_f64(id as f64),
                    );
                    EVENT_CLOSURES.with(|v| v.borrow_mut().insert(id, node_closures));
                }
            }

            for child in &el.children {
                if let Some(child_node) = create_dom_node(child) {
                    if let Err(err) = element.append_child(&child_node) {
                        console::error_1(
                             &format!(
                                 "{{\"scope\":\"create_dom_node\",\"detail\":\"append child failed\",\"error\":\"{:?}\"}}",
                                 err
                             )
                             .into(),
                         );
                    }
                }
            }
            Some(element.into())
        }
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
            // For nested dynamic nodes in newly created trees
            let anchor = document.create_comment("dynamic-anchor");
            let anchor_node: WebNode = anchor.clone().into();

            let current_node = Rc::new(RefCell::new(anchor_node.clone()));
            let current_vnode = Rc::new(RefCell::new(None));
            let f = f.clone();

            // Build initial content through an effect so dependency tracking and first render
            // share the same execution path.

            let first_run = Rc::new(Cell::new(true));

            // `create_effect` runs immediately; capture the first produced node for the
            // synchronous return value while keeping a mutable pointer for later replacements.

            let initial_node = Rc::new(RefCell::new(None));
            let initial_node_clone = initial_node.clone();

            create_effect(move || {
                let new_v_node = f();

                if first_run.get() {
                    let Some(new_dom_node) = create_dom_node(&new_v_node) else {
                        console::error_1(&"{\"scope\":\"create_dom_node\",\"detail\":\"dynamic node creation failed\"}".into());
                        return;
                    };
                    first_run.set(false);
                    *initial_node_clone.borrow_mut() = Some(new_dom_node.clone());
                    // Store node for subsequent dynamic replacements.
                    *current_node.borrow_mut() = new_dom_node;
                    *current_vnode.borrow_mut() = Some(new_v_node);
                    return;
                }

                // Update
                let old = current_node.borrow();
                let old_v = current_vnode.borrow();

                if let Some(parent) = old.parent_node() {
                    let mut patched = false;
                    if let Some(old_vnode) = old_v.as_ref() {
                        if let Some(patched_node) = patch_dom(&parent, &old, old_vnode, &new_v_node)
                        {
                            *current_node.borrow_mut() = patched_node;
                            patched = true;
                        }
                    }

                    if !patched {
                        if let Some(new_dom_node) = create_dom_node(&new_v_node) {
                            remove_dom_node_closures(&old);
                            if let Err(err) = parent.replace_child(&new_dom_node, &old) {
                                console::error_1(
                                    &format!(
                                        "{{\"scope\":\"create_dom_node\",\"detail\":\"dynamic replace_child failed\",\"error\":\"{:?}\"}}",
                                        err
                                    )
                                    .into(),
                                );
                                return;
                            }
                            *current_node.borrow_mut() = new_dom_node;
                        }
                    }
                }
                drop(old_v);
                *current_vnode.borrow_mut() = Some(new_v_node);
            });

            // Return the initial node produced by the first effect run.
            let result = initial_node.borrow().clone().unwrap_or_else(|| {
                // Defensive fallback for an unexpected empty initial render.
                document.create_comment("empty-dynamic").into()
            });

            Some(result)
        }
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    console::log_1(&"Krab Client initialized".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use krab_core::{Attribute, Element, Node};

    fn element(tag: &str, children: Vec<Node>) -> Node {
        element_with_attributes(tag, vec![], children)
    }

    fn element_with_attributes(tag: &str, attributes: Vec<Attribute>, children: Vec<Node>) -> Node {
        Node::Element(Element {
            tag: tag.to_string(),
            attributes,
            children,
            events: vec![],
        })
    }

    fn hydration_id_attr(value: &str) -> Attribute {
        Attribute::new(
            krab_core::HYDRATION_NODE_ID_ATTR.to_string(),
            value.to_string(),
        )
    }

    #[test]
    fn hydration_plan_marks_matching_element_as_reusable() {
        let expected = element("div", vec![Node::Text("hello".to_string())]);
        let actual = element("div", vec![Node::Text("hello".to_string())]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Reuse));
        assert!(plan
            .children
            .iter()
            .all(|child| matches!(child.outcome, HydrationOutcome::Reuse)));
    }

    #[test]
    fn hydration_plan_marks_tag_mismatch_as_replace() {
        let expected = element("button", vec![]);
        let actual = element("div", vec![]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Replace));
        assert_eq!(plan.reason, Some("element_tag_mismatch"));
    }

    #[test]
    fn hydration_plan_prefers_marker_mismatch_over_tag_match() {
        let expected =
            element_with_attributes("div", vec![hydration_id_attr("boundary:1/0")], vec![]);
        let actual =
            element_with_attributes("div", vec![hydration_id_attr("boundary:1/1")], vec![]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Replace));
        assert_eq!(plan.reason, Some("element_marker_mismatch"));
    }

    #[test]
    fn hydration_plan_marks_missing_dom_node_as_append() {
        let expected = element("span", vec![Node::Text("late".to_string())]);

        let plan = hydration_plan(&expected, None);

        assert!(matches!(plan.outcome, HydrationOutcome::Append));
        assert_eq!(plan.reason, Some("missing_dom_node"));
    }

    #[test]
    fn hydration_plan_marks_text_mismatch_as_patch_text() {
        let expected = Node::Text("new".to_string());
        let actual = Node::Text("old".to_string());

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::PatchText));
        assert_eq!(plan.reason, Some("text_content_mismatch"));
    }

    #[test]
    fn hydration_plan_marks_expected_text_against_element_as_replace() {
        let expected = Node::Text("text".to_string());
        let actual = element("span", vec![]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Replace));
        assert_eq!(plan.reason, Some("expected_text_found_non_text_node"));
    }

    #[test]
    fn hydration_plan_marks_fragment_with_missing_child_as_append() {
        let expected = Node::Fragment(vec![
            element("div", vec![]),
            Node::Text("second".to_string()),
        ]);
        let actual = Node::Fragment(vec![element("div", vec![])]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Reuse));
        assert_eq!(plan.children.len(), 2);
        assert!(matches!(plan.children[1].outcome, HydrationOutcome::Append));
    }

    #[test]
    fn hydration_plan_counts_dynamic_nodes_as_replace_boundary() {
        let expected = Node::Dynamic(std::rc::Rc::new(|| Node::Text("next".to_string())));
        let actual = Node::Text("prev".to_string());

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Replace));
        assert_eq!(plan.reason, Some("dynamic_node_boundary"));
    }

    #[test]
    fn hydration_plan_ignores_attribute_differences_for_reuse() {
        let expected = Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![Attribute::new("class".to_string(), "new".to_string())],
            children: vec![],
            events: vec![],
        });
        let actual = Node::Element(Element {
            tag: "div".to_string(),
            attributes: vec![Attribute::new("class".to_string(), "old".to_string())],
            children: vec![],
            events: vec![],
        });

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Reuse));
    }

    #[test]
    fn hydration_plan_marks_extra_dom_child_as_remove() {
        let expected = Node::Fragment(vec![element("div", vec![])]);
        let actual = Node::Fragment(vec![
            element("div", vec![]),
            Node::Text("extra".to_string()),
        ]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Reuse));
        assert_eq!(plan.children.len(), 2);
        assert!(matches!(plan.children[1].outcome, HydrationOutcome::Remove));
        assert_eq!(plan.children[1].reason, Some("unexpected_dom_node"));
    }

    #[test]
    fn hydration_plan_marks_reordered_marker_matched_child_as_reorder() {
        let expected = Node::Fragment(vec![
            element_with_attributes(
                "div",
                vec![hydration_id_attr("boundary:1/0")],
                vec![Node::Text("first".to_string())],
            ),
            element_with_attributes(
                "div",
                vec![hydration_id_attr("boundary:1/1")],
                vec![Node::Text("second".to_string())],
            ),
        ]);
        let actual = Node::Fragment(vec![
            element_with_attributes(
                "div",
                vec![hydration_id_attr("boundary:1/1")],
                vec![Node::Text("second".to_string())],
            ),
            element_with_attributes(
                "div",
                vec![hydration_id_attr("boundary:1/0")],
                vec![Node::Text("first".to_string())],
            ),
        ]);

        let plan = hydration_plan(&expected, Some(&actual));

        assert!(matches!(plan.outcome, HydrationOutcome::Reuse));
        assert_eq!(plan.children.len(), 2);
        assert!(matches!(
            plan.children[0].outcome,
            HydrationOutcome::Reorder
        ));
        assert_eq!(plan.children[0].reason, Some("marker_reordered_dom_node"));
        assert!(matches!(plan.children[1].outcome, HydrationOutcome::Reuse));
    }

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

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum HydrationOutcome {
    Reuse,
    Reorder,
    Replace,
    Append,
    Remove,
    PatchText,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct HydrationPlan {
    outcome: HydrationOutcome,
    reason: Option<&'static str>,
    children: Vec<HydrationPlan>,
}

#[cfg(test)]
fn hydration_plan_children(
    expected_children: &[Node],
    actual_children: &[Node],
) -> Vec<HydrationPlan> {
    let mut remaining_actual: Vec<Option<&Node>> = actual_children.iter().map(Some).collect();
    let mut cursor = 0usize;
    let mut children = Vec::with_capacity(expected_children.len().max(actual_children.len()));

    for expected_child in expected_children {
        if let Some(expected_id) = node_hydration_id(expected_child) {
            if let Some(found_index) = (cursor..remaining_actual.len()).find(|index| {
                remaining_actual[*index].and_then(node_hydration_id) == Some(expected_id)
            }) {
                let actual_child = remaining_actual.remove(found_index).expect("matched child");
                remaining_actual.insert(cursor, None);

                let mut plan = hydration_plan(expected_child, Some(actual_child));
                if found_index != cursor && matches!(plan.outcome, HydrationOutcome::Reuse) {
                    plan.outcome = HydrationOutcome::Reorder;
                    plan.reason = Some("marker_reordered_dom_node");
                }
                children.push(plan);
                cursor += 1;
                continue;
            }
        }

        if cursor < remaining_actual.len() {
            let actual_child = remaining_actual[cursor].take();
            children.push(hydration_plan(expected_child, actual_child));
            cursor += 1;
        } else {
            children.push(hydration_plan(expected_child, None));
        }
    }

    for extra_child in remaining_actual.into_iter().flatten() {
        children.push(hydration_plan(&Node::Fragment(vec![]), Some(extra_child)));
    }

    children
}

#[cfg(test)]
fn hydration_plan(expected: &Node, actual: Option<&Node>) -> HydrationPlan {
    match (expected, actual) {
        (_, None) => HydrationPlan {
            outcome: HydrationOutcome::Append,
            reason: Some("missing_dom_node"),
            children: vec![],
        },
        (Node::Text(expected_text), Some(Node::Text(actual_text))) => HydrationPlan {
            outcome: if expected_text == actual_text {
                HydrationOutcome::Reuse
            } else {
                HydrationOutcome::PatchText
            },
            reason: if expected_text == actual_text {
                None
            } else {
                Some("text_content_mismatch")
            },
            children: vec![],
        },
        (Node::Text(_), Some(_)) => HydrationPlan {
            outcome: HydrationOutcome::Replace,
            reason: Some("expected_text_found_non_text_node"),
            children: vec![],
        },
        (Node::Element(expected_el), Some(Node::Element(actual_el))) => {
            if let (Some(expected_id), Some(actual_id)) = (
                element_hydration_id(expected_el),
                element_hydration_id(actual_el),
            ) {
                if expected_id != actual_id {
                    return HydrationPlan {
                        outcome: HydrationOutcome::Replace,
                        reason: Some("element_marker_mismatch"),
                        children: vec![],
                    };
                }
            }

            if expected_el.tag != actual_el.tag {
                HydrationPlan {
                    outcome: HydrationOutcome::Replace,
                    reason: Some("element_tag_mismatch"),
                    children: vec![],
                }
            } else {
                HydrationPlan {
                    outcome: HydrationOutcome::Reuse,
                    reason: None,
                    children: hydration_plan_children(&expected_el.children, &actual_el.children),
                }
            }
        }
        (Node::Element(_), Some(_)) => HydrationPlan {
            outcome: HydrationOutcome::Replace,
            reason: Some("expected_element_found_non_element_node"),
            children: vec![],
        },
        (Node::Fragment(expected_children), Some(_)) if expected_children.is_empty() => {
            HydrationPlan {
                outcome: HydrationOutcome::Remove,
                reason: Some("unexpected_dom_node"),
                children: vec![],
            }
        }
        (Node::Fragment(expected_children), Some(Node::Fragment(actual_children))) => {
            HydrationPlan {
                outcome: HydrationOutcome::Reuse,
                reason: None,
                children: hydration_plan_children(expected_children, actual_children),
            }
        }
        (Node::Fragment(expected_children), Some(actual_node)) => HydrationPlan {
            outcome: HydrationOutcome::Reuse,
            reason: None,
            children: expected_children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    hydration_plan(child, if index == 0 { Some(actual_node) } else { None })
                })
                .collect(),
        },
        (Node::Dynamic(_), Some(_)) => HydrationPlan {
            outcome: HydrationOutcome::Replace,
            reason: Some("dynamic_node_boundary"),
            children: vec![],
        },
    }
}
