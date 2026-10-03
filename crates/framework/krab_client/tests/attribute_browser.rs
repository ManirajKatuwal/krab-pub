//! Browser tests for reactive attributes (ADR 0015).
//!
//! Run with the same command as `hydration_browser`; see that file's docs.
//!
//! What is under test is DOM state an HTML comparison cannot see: that an
//! effect patches the *live* attribute and, for form controls, the live
//! property; that hydration adopts a correct server attribute without writing
//! it; and that the effects are released with the element.

#![cfg(target_arch = "wasm32")]

use krab_core::signal::{batch, create_signal, ReadSignal, WriteSignal};
use krab_core::Node;
use krab_macros::view;
use std::cell::RefCell;
use wasm_bindgen::JsCast as _;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlInputElement};

#[path = "support/mod.rs"]
mod support;

wasm_bindgen_test_configure!(run_in_browser);

const ROOT_ID: &str = "krab-attribute-root";

fn mount_built(node: &Node) -> Element {
    let root = support::mount_container(ROOT_ID);
    let built = krab_client::build_dom_for_test(node).expect("build failed");
    root.append_child(&built).expect("append");
    root
}

fn first(root: &Element, selector: &str) -> Element {
    root.query_selector(selector)
        .expect("query failed")
        .expect("element missing")
}

#[wasm_bindgen_test]
fn a_created_element_tracks_its_dynamic_attribute() {
    let (label, set_label) = create_signal("one".to_string());
    let root = mount_built(&view! { <div title={move || label.get()}></div> });
    let div = first(&root, "div");

    assert_eq!(div.get_attribute("title").as_deref(), Some("one"));
    batch(|| set_label.set("two".to_string()));
    assert_eq!(div.get_attribute("title").as_deref(), Some("two"));
}

#[wasm_bindgen_test]
fn a_boolean_attribute_is_added_and_removed() {
    let (busy, set_busy) = create_signal(false);
    let root = mount_built(&view! { <button disabled={move || busy.get()}>"Go"</button> });
    let button = first(&root, "button");

    assert!(!button.has_attribute("disabled"));
    batch(|| set_busy.set(true));
    assert_eq!(button.get_attribute("disabled").as_deref(), Some(""));
    batch(|| set_busy.set(false));
    assert!(!button.has_attribute("disabled"));
}

/// The attribute is only the default for a form control; once the user has
/// typed, only the property changes what is shown.
#[wasm_bindgen_test]
fn value_and_checked_are_written_to_the_live_property() {
    let (text, set_text) = create_signal("a".to_string());
    let (on, set_on) = create_signal(false);
    let root = mount_built(&view! {
        <div>
            <input id="t" value={move || text.get()}/>
            <input id="c" r#type="checkbox" checked={move || on.get()}/>
        </div>
    });
    let input: HtmlInputElement = first(&root, "#t").dyn_into().expect("input");
    let checkbox: HtmlInputElement = first(&root, "#c").dyn_into().expect("checkbox");

    // Simulate user input: this sets the property and the dirty flag, after
    // which the attribute no longer drives the displayed value.
    input.set_value("typed");
    batch(|| set_text.set("from signal".to_string()));
    assert_eq!(input.value(), "from signal");

    assert!(!checkbox.checked());
    batch(|| set_on.set(true));
    assert!(checkbox.checked());
    batch(|| set_on.set(false));
    assert!(!checkbox.checked());
}

#[wasm_bindgen_test]
fn removing_the_element_releases_its_attribute_effects() {
    let before = krab_client::attribute_effect_count();
    let (label, _set_label) = create_signal("x".to_string());
    let root = mount_built(&view! { <p title={move || label.get()}>"p"</p> });
    assert_eq!(krab_client::attribute_effect_count(), before + 1);

    krab_client::unmount(&root);
    assert_eq!(krab_client::attribute_effect_count(), before);
}

// ── Hydration ────────────────────────────────────────────────────────────────

thread_local! {
    static CLASS: RefCell<Option<(ReadSignal<String>, WriteSignal<String>)>> =
        const { RefCell::new(None) };
}

fn class_signal() -> (ReadSignal<String>, WriteSignal<String>) {
    CLASS.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| create_signal("ready".to_string()))
            .clone()
    })
}

fn attribute_island(_props: String) -> Node {
    let (class, _) = class_signal();
    view! { <span class={move || class.get()}>"x"</span> }
}

inventory::submit! {
    krab_client::IslandDefinition { name: "AttributeIsland", factory: attribute_island }
}

fn mount_island(server_class: &str) -> Element {
    let root = support::mount_container(ROOT_ID);
    root.set_inner_html(&format!(
        concat!(
            r#"<div data-island="AttributeIsland" data-props='{{}}' "#,
            r#"data-krab-boundary="AttributeIsland" data-krab-boundary-id="ba" "#,
            r#"data-krab-boundary-state="ssr">"#,
            r#"<span class="{}" data-krab-node-id="ba/0">x</span>"#,
            r#"</div>"#
        ),
        server_class
    ));
    root
}

/// SSR and hydration agree on the initial value, so hydration adopts the
/// element and writes nothing: a `MutationObserver` watching the attribute
/// records no mutation across the hydration pass.
#[wasm_bindgen_test]
fn hydration_adopts_a_matching_attribute_without_rewriting_it() {
    let (_class, set_class) = class_signal();
    batch(|| set_class.set("ready".to_string()));

    let root = mount_island("ready");
    let span = first(&root, "span");

    let noop = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(|| {});
    let observer = web_sys::MutationObserver::new(noop.as_ref().unchecked_ref()).expect("observer");
    let options = web_sys::MutationObserverInit::new();
    options.set_attributes(true);
    observer
        .observe_with_options(&span, &options)
        .expect("observe");

    krab_client::hydrate_within(&root);

    // `takeRecords` drains synchronously, so nothing is lost to the microtask
    // the observer's callback would otherwise run in.
    let records = observer.take_records();
    observer.disconnect();
    assert_eq!(
        records.length(),
        0,
        "a matching attribute must not be rewritten during hydration"
    );

    let island = first(&root, "[data-island]");
    assert_eq!(
        island.get_attribute("data-krab-boundary-state").as_deref(),
        Some("ok")
    );

    // And it is live afterwards.
    batch(|| set_class.set("busy".to_string()));
    assert_eq!(span.get_attribute("class").as_deref(), Some("busy"));
    krab_client::unmount(&root);
}

#[wasm_bindgen_test]
fn hydration_corrects_a_disagreeing_attribute() {
    let (_class, set_class) = class_signal();
    batch(|| set_class.set("client".to_string()));

    let root = mount_island("server");
    krab_client::hydrate_within(&root);

    assert_eq!(
        first(&root, "span").get_attribute("class").as_deref(),
        Some("client")
    );
    krab_client::unmount(&root);
}
