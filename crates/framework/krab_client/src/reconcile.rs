//! Building and patching DOM for [`Node::Dynamic`] regions after hydration.
//!
//! Hydration adopts server-rendered nodes; everything that happens afterwards —
//! a signal change re-rendering a region, a keyed list reordering, a branch
//! swapping — goes through the reconciler in this module. It is also the only
//! code that creates DOM from a vnode, which is why the hydration walk calls
//! back into it when a node is missing or has the wrong shape.

use crate::hydration::node_hydration_id;
use crate::resources::{
    attach_element_events, bind_dynamic_attributes, dispose_region_effect,
    own_region_effect_by_enclosing_effect, register_dynamic_region, release_dom_node_resources,
    take_dynamic_region,
};
use krab_core::signal::{create_effect_scoped, EffectHandle};
use krab_core::Node;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{console, Element, Node as WebNode};

/// Replace `old` with a freshly built node for `v_node`, releasing everything
/// `old`'s subtree retains first so it is not leaked.
pub(crate) fn replace_dom_node(parent: &WebNode, old: &WebNode, v_node: &Node, scope: &str) {
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
pub(crate) fn append_dom_node(parent: &WebNode, v_node: &Node, scope: &str) {
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
#[derive(Clone)]
pub(crate) struct DynamicRegionCells {
    /// The tracked run: one node per flattened child, region anchors standing
    /// in for nested regions.
    pub(crate) rendered: Rc<RefCell<Vec<WebNode>>>,
    pub(crate) current_vnode: Rc<RefCell<Option<Node>>>,
    /// The trailing comment bounding the run. Pre-filled on the mount path;
    /// created on first update on the hydration path (inserting during the
    /// hydration traversal would shift the live `NodeList`).
    pub(crate) anchor: Rc<RefCell<Option<WebNode>>>,
    /// Where a lazily-created anchor belongs when the run is empty: the parent
    /// element and the sibling the run precedes. Without this, an empty SSR
    /// render had no reference point and the region could never show anything.
    pub(crate) empty_position: Rc<RefCell<Option<EmptyRunPosition>>>,
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
    pub(crate) effect: Rc<RefCell<Option<EffectHandle>>>,
}

/// The parent element and the following sibling an empty run sits before.
type EmptyRunPosition = (Element, Option<WebNode>);

/// Apply one new render of a dynamic region to the DOM.
pub(crate) fn update_dynamic_region(cells: &DynamicRegionCells, new_v_node: Node) {
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

                // `Some(result)` when there was a position to insert at, `None`
                // when there was not. A DOM `insertBefore` can itself fail (for
                // instance a `HierarchyRequestError` when the parent is a node
                // type that cannot hold a comment); that used to be discarded
                // and reported as success, leaving `anchor` pointing at a
                // comment that was never in the document.
                let inserted = if let Some(last) = previous_nodes.last() {
                    // After the run's current last node.
                    last.parent_node().map(|parent| {
                        parent
                            .insert_before(&created, last.next_sibling().as_ref())
                            .is_ok()
                    })
                } else if let Some((parent, next)) = cells.empty_position.borrow().as_ref() {
                    // Empty run: use the position captured at hydration. The
                    // captured next-sibling may have been replaced since; fall
                    // back to appending rather than guessing.
                    let next_ok = next
                        .as_ref()
                        .filter(|n| n.parent_node().as_deref() == Some(parent.as_ref()));
                    Some(parent.insert_before(&created, next_ok).is_ok())
                } else {
                    None
                };

                match inserted {
                    Some(true) => {}
                    Some(false) => {
                        console::warn_1(
                            &"{\"scope\":\"dynamic_region\",\"detail\":\"anchor insert failed; region update skipped\"}"
                                .into(),
                        );
                        *cells.current_vnode.borrow_mut() = Some(new_v_node);
                        return;
                    }
                    None => {
                        // No position to anchor to: give up rather than guess.
                        *cells.current_vnode.borrow_mut() = Some(new_v_node);
                        return;
                    }
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
        (Node::Comment(old_text), Node::Comment(new_text)) => {
            if old_text != new_text {
                real_node.set_text_content(Some(new_text));
            }
            Some(real_node.clone())
        }
        (Node::Element(old_el), Node::Element(new_el)) if old_el.tag == new_el.tag => {
            let el = real_node.dyn_ref::<Element>()?;

            // Attributes. Static ones are diffed here; dynamic ones are left
            // to the re-bind below, whose first run writes the new source's
            // value. An attribute that went from dynamic to static (or back)
            // is covered by the same two paths.
            for old_attr in &old_el.attributes {
                if !new_el.attributes.iter().any(|a| a.name == old_attr.name) {
                    let _ = el.remove_attribute(&old_attr.name);
                }
            }
            for new_attr in new_el.attributes.iter().filter(|a| !a.is_dynamic()) {
                let old_attr = old_el
                    .attributes
                    .iter()
                    .find(|a| a.name == new_attr.name && !a.is_dynamic());
                if old_attr.map(|a| &a.value) != Some(&new_attr.value) {
                    let _ = el.set_attribute(&new_attr.name, &new_attr.value);
                }
            }
            // The reused element keeps the effects its creation render bound,
            // whose sources capture that render's state, unless swapped here —
            // the same reasoning as the listeners below.
            if old_el.attributes.iter().any(|a| a.is_dynamic())
                || new_el.attributes.iter().any(|a| a.is_dynamic())
            {
                bind_dynamic_attributes(el, new_el, false);
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

    // Dynamic attributes are written by their effect, bound below.
    for attr in el.attributes.iter().filter(|a| !a.is_dynamic()) {
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
    bind_dynamic_attributes(&element, el, false);

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

fn create_dom_node(v_node: &Node) -> Option<WebNode> {
    let document = web_sys::window().and_then(|w| w.document())?;

    match v_node {
        Node::Element(el) => create_element_node(&document, el),
        Node::Text(text) => Some(document.create_text_node(text).into()),
        Node::Comment(text) => Some(document.create_comment(text).into()),
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
/// in the DOM for the region's life and is registered in `DYNAMIC_REGIONS`.
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
fn create_run_nodes(node: &Node) -> Vec<(WebNode, WebNode)> {
    let mut flat: Vec<&Node> = Vec::new();
    flatten_children(std::slice::from_ref(node), &mut flat);
    flat.iter()
        .filter_map(|child| create_tracked_child(child))
        .collect()
}

/// If `node` is a region anchor, tear the region down: dispose its effect and
/// remove its rendered content (its sibling nodes) from `el`, recursively
/// handling regions nested inside it. The anchor itself is left for the caller
/// to remove or replace.
///
/// Unlike [`release_dom_node_resources`], this removes DOM — the region's run
/// lives *beside* the anchor, so replacing the anchor alone would strand it.
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
fn remove_tracked_node(el: &Element, node: &WebNode) {
    remove_region_content(el, node);
    release_dom_node_resources(node);
    let _ = el.remove_child(node);
}
