//! The two registries hydration and reconciliation both write to, and their
//! release.
//!
//! Event-listener closures are owned here because dropping one invalidates the
//! JS function pointer behind a live listener, and `forget()`ing one leaks it on
//! every re-render. Dynamic regions are registered here so that a parent which
//! tracks a nested region by its anchor comment can still reach the region's
//! content and effect. Both are released by one subtree walk,
//! [`release_dom_node_resources`], so neither can be forgotten on a removal path.

use crate::reconcile::DynamicRegionCells;
use krab_core::signal::{create_effect_scoped, on_cleanup, EffectHandle};
use std::cell::{Cell, RefCell};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{console, Element, Node as WebNode};

type EventClosure = Closure<dyn FnMut(web_sys::Event)>;
type EventClosureMap = std::collections::HashMap<u32, Vec<(String, EventClosure)>>;
type DynamicRegionMap = std::collections::HashMap<u32, DynamicRegionCells>;
type AttributeEffectMap = std::collections::HashMap<u32, Vec<EffectHandle>>;

// Holds event-listener closures for the lifetime of the page or node so they are not
// dropped (which would invalidate the JS function pointer) but also not
// silently leaked via forget().
thread_local! {
    static EVENT_CLOSURES: RefCell<EventClosureMap> = RefCell::new(EventClosureMap::new());
    static NEXT_DOM_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
    /// Every live [`Node::Dynamic`] region, keyed by the `__krab_region`
    /// expando on its anchor comment. A parent that tracks a nested region by
    /// its anchor uses this to reach the region's *content* and its effect —
    /// the anchor alone cannot reach either.
    static DYNAMIC_REGIONS: RefCell<DynamicRegionMap> = RefCell::new(DynamicRegionMap::new());
    /// The effects behind an element's reactive attributes (ADR 0015), keyed
    /// by the `__krab_attr_id` expando on the element. Held here for the same
    /// reason event closures are: the element is the unit that is replaced or
    /// removed, and one subtree walk has to be able to find everything it
    /// keeps alive.
    static ATTRIBUTE_EFFECTS: RefCell<AttributeEffectMap> = RefCell::new(AttributeEffectMap::new());
}

/// Number of DOM elements currently holding reactive-attribute effects.
#[doc(hidden)]
pub fn attribute_effect_count() -> usize {
    ATTRIBUTE_EFFECTS.with(|effects| effects.borrow().len())
}

/// Dispose the reactive-attribute effects bound to `real_el`, if any.
fn detach_dynamic_attributes(real_el: &Element) {
    let Some(id) = node_expando_id(real_el.as_ref(), "__krab_attr_id") else {
        return;
    };
    let Some(handles) = ATTRIBUTE_EFFECTS.with(|effects| effects.borrow_mut().remove(&id)) else {
        return;
    };
    for handle in &handles {
        handle.dispose();
    }
}

/// Bind `v_el`'s reactive attributes to `real_el`: one scoped effect per
/// dynamic attribute, re-evaluating its source and patching the DOM.
///
/// Any binding already on the element is disposed first, as with
/// [`attach_element_events`], so re-binding a reused element swaps the
/// generation instead of stacking a second effect on the first.
///
/// `hydrating` marks the adoption of server-rendered markup. The first run
/// then writes only where the DOM disagrees with the source — a correct
/// attribute is left untouched — and never writes a form control's live
/// property, which may already hold what the user typed before the bundle
/// loaded. Every later run, and every run on a freshly created element,
/// writes both.
///
/// The effects are `create_effect_scoped` roots rather than children of an
/// enclosing region effect: their lifetime is the element's, and they are
/// released by [`release_dom_node_resources`] or the next re-bind.
pub(crate) fn bind_dynamic_attributes(
    real_el: &Element,
    v_el: &krab_core::Element,
    hydrating: bool,
) {
    detach_dynamic_attributes(real_el);

    let mut handles = Vec::new();
    for attr in &v_el.attributes {
        let Some(source) = attr.dynamic.clone() else {
            continue;
        };
        let element = real_el.clone();
        let name = attr.name.clone();
        let adopting = Cell::new(hydrating);
        handles.push(create_effect_scoped(move || {
            let value = source();
            apply_dynamic_attribute(&element, &name, value.as_deref(), adopting.replace(false));
        }));
    }

    if handles.is_empty() {
        return;
    }

    let id = NEXT_DOM_ID.with(|id| {
        let v = id.get();
        id.set(v + 1);
        v
    });
    let _ = js_sys::Reflect::set(
        real_el.as_ref(),
        &JsValue::from_str("__krab_attr_id"),
        &JsValue::from_f64(id as f64),
    );
    ATTRIBUTE_EFFECTS.with(|effects| effects.borrow_mut().insert(id, handles));
}

/// Write one reactive attribute's value: `Some` sets it, `None` removes it.
///
/// The attribute is compared before it is written, so an unchanged value costs
/// a read and no mutation (and no `MutationObserver` record).
///
/// Three attributes are only *defaults* once a form control exists: `value` on
/// `input`/`textarea`/`select`, `checked` on `input`, `selected` on `option`.
/// After the user edits the control, changing the attribute no longer changes
/// what it shows, so the live DOM property is written too — except while
/// adopting server markup (`adopting`), where the property may hold input the
/// user made before hydration and the attribute already matches the server.
fn apply_dynamic_attribute(element: &Element, name: &str, value: Option<&str>, adopting: bool) {
    if element.get_attribute(name).as_deref() != value {
        let result = match value {
            Some(value) => element.set_attribute(name, value),
            None => element.remove_attribute(name),
        };
        if let Err(err) = result {
            console::error_1(
                &format!(
                    "{{\"scope\":\"dynamic_attribute\",\"detail\":\"attribute write failed\",\"attribute\":\"{name}\",\"error\":\"{err:?}\"}}"
                )
                .into(),
            );
        }
    }

    if adopting {
        return;
    }

    let tag = element.tag_name().to_ascii_lowercase();
    let property: Option<(&str, JsValue)> = match (tag.as_str(), name) {
        ("input" | "textarea" | "select", "value") => {
            Some(("value", JsValue::from_str(value.unwrap_or(""))))
        }
        ("input", "checked") => Some(("checked", JsValue::from_bool(value.is_some()))),
        ("option", "selected") => Some(("selected", JsValue::from_bool(value.is_some()))),
        _ => None,
    };
    if let Some((key, wanted)) = property {
        let key = JsValue::from_str(key);
        // Compared first: re-assigning an unchanged `value` can move the caret
        // in some browsers.
        let current = js_sys::Reflect::get(element.as_ref(), &key).unwrap_or(JsValue::UNDEFINED);
        if current != wanted {
            let _ = js_sys::Reflect::set(element.as_ref(), &key, &wanted);
        }
    }
}

/// Number of DOM elements currently holding retained event closures.
#[doc(hidden)]
pub fn event_closure_count() -> usize {
    EVENT_CLOSURES.with(|closures| closures.borrow().len())
}

/// Number of dynamic regions currently registered against a live anchor.
#[doc(hidden)]
pub fn dynamic_region_count() -> usize {
    DYNAMIC_REGIONS.with(|regions| regions.borrow().len())
}

/// Detach every listener previously attached by [`attach_element_events`].
///
/// The JS listener is removed *before* its closure is dropped — the reverse
/// order leaves a live listener whose Rust side is gone, which throws
/// "closure invoked after being dropped" on the next event.
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
pub(crate) fn attach_element_events(real_el: &Element, v_el: &krab_core::Element, scope: &str) {
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

/// Read a `u32` expando written by [`attach_element_events`] or
/// [`register_dynamic_region`]. One reader for both, so the two registries
/// cannot drift apart in how they identify a node.
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
/// invisible, but [`unmount`](crate::unmount) deliberately *leaves* its subtree in place:
/// dropping a closure whose listener is still bound makes the next event throw
/// "closure invoked after being dropped".
///
/// A region *anchored* in this subtree has its rendered run in the subtree too
/// (the run is the anchor's preceding siblings), so the caller's own DOM removal
/// disposes of the nodes; unregistering is all that is needed here. An anchor
/// whose content lies *outside* the discarded subtree — the anchor itself being
/// replaced — is the separate job of `reconcile::remove_region_content`.
pub(crate) fn release_dom_node_resources(node: &WebNode) {
    if let Some(element) = node.dyn_ref::<Element>() {
        detach_element_events(element);
        detach_dynamic_attributes(element);
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

/// Register a dynamic region under its anchor.
///
/// The map holds a *clone* of the cells, which shares every `Rc` with the live
/// region — including the effect slot, so a handle stored after registration is
/// still visible here.
pub(crate) fn register_dynamic_region(anchor: &WebNode, cells: &DynamicRegionCells) {
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
pub(crate) fn take_dynamic_region(node: &WebNode) -> Option<DynamicRegionCells> {
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
pub(crate) fn dispose_region_effect(cells: &DynamicRegionCells) {
    let handle = cells.effect.borrow_mut().take();
    if let Some(handle) = handle {
        handle.dispose();
    }
}

/// Hand this region's effect to the effect currently running, if there is one.
///
/// [`create_effect_scoped`](krab_core::signal::create_effect_scoped) deliberately never adopts a parent — its lifetime
/// is the handle's — but a region built *during* another region's render must
/// still die when that render is superseded. Registering the disposal as an
/// `on_cleanup` on the enclosing effect restores exactly the ownership that
/// plain `create_effect` gave for free, without giving up the handle. Outside an
/// effect this is a no-op and the region lives until it is unmounted or its
/// anchor is removed.
pub(crate) fn own_region_effect_by_enclosing_effect(cells: &DynamicRegionCells) {
    let effect = cells.effect.clone();
    on_cleanup(move || {
        let handle = effect.borrow_mut().take();
        if let Some(handle) = handle {
            handle.dispose();
        }
    });
}
