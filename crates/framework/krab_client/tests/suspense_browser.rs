//! Browser tests for `<Suspense>` (ADR 0016) and the streamed-content hook
//! (ADR 0017).
//!
//! Run with the same command as `hydration_browser`; see that file's docs.

#![cfg(target_arch = "wasm32")]

use krab_core::signal::{batch, create_signal, ReadSignal, WriteSignal};
use krab_core::suspense::use_suspense;
use krab_core::{Node, Render};
use krab_macros::view;
use std::cell::RefCell;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

#[path = "support/islands.rs"]
mod islands;
#[path = "support/mod.rs"]
mod support;

wasm_bindgen_test_configure!(run_in_browser);

const ROOT_ID: &str = "krab-suspense-root";

/// A boundary whose one source is `!ready`: pending until `ready` is set.
fn boundary(ready: ReadSignal<bool>) -> Node {
    krab_core::signal::with_owner(move || {
        view! {
            <div class="wrap">
                <Suspense fallback={|| view! { <p class="fallback">"Loading"</p> }}>
                    {{
                        if let Some(context) = use_suspense() {
                            context.register(move || !ready.get());
                        }
                        view! { <p class="content">"Loaded"</p> }
                    }}
                </Suspense>
            </div>
        }
    })
}

#[wasm_bindgen_test]
fn a_mounted_boundary_swaps_from_fallback_to_content() {
    let (ready, set_ready) = create_signal(false);
    let root = support::mount_container(ROOT_ID);
    let built = krab_client::build_dom_for_test(&boundary(ready)).expect("build");
    root.append_child(&built).expect("append");

    assert!(root.query_selector(".fallback").unwrap().is_some());
    assert!(root.query_selector(".content").unwrap().is_none());

    batch(|| set_ready.set(true));

    assert!(root.query_selector(".fallback").unwrap().is_none());
    assert!(
        root.query_selector(".content").unwrap().is_some(),
        "{}",
        root.inner_html()
    );
    // The markers travel with the region: still one opening marker.
    assert_eq!(root.inner_html().matches(":pending-->").count(), 1);
}

thread_local! {
    static READY: RefCell<Option<(ReadSignal<bool>, WriteSignal<bool>)>> =
        const { RefCell::new(None) };
}

fn ready_signal() -> (ReadSignal<bool>, WriteSignal<bool>) {
    READY.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| create_signal(false))
            .clone()
    })
}

fn suspense_island(_props: String) -> Node {
    boundary(ready_signal().0)
}

inventory::submit! {
    krab_client::IslandDefinition { name: "SuspenseIsland", factory: suspense_island }
}

/// SSR renders the fallback with comment markers around it; hydration must
/// adopt the markers (whose boundary ids differ between the two renders)
/// without counting a mismatch, and the boundary must still switch.
#[wasm_bindgen_test]
fn a_hydrated_boundary_adopts_its_markers_and_switches() {
    let (_ready, set_ready) = ready_signal();
    batch(|| set_ready.set(false));

    // The server half, rendered here: the same tree the factory builds.
    let (server_ready, _) = create_signal(false);
    let server_html = krab_core::annotate_hydration_tree(boundary(server_ready), "bs").render();

    let root = support::mount_container(ROOT_ID);
    root.set_inner_html(&format!(
        concat!(
            r#"<div data-island="SuspenseIsland" data-props='{{}}' "#,
            r#"data-krab-boundary="SuspenseIsland" data-krab-boundary-id="bs" "#,
            r#"data-krab-boundary-state="ssr">{}</div>"#
        ),
        server_html
    ));
    krab_client::hydrate_within(&root);

    let island = root
        .query_selector("[data-island]")
        .unwrap()
        .expect("island");
    assert_eq!(
        island.get_attribute("data-krab-boundary-state").as_deref(),
        Some("ok"),
        "{}",
        island.inner_html()
    );
    assert!(root.query_selector(".fallback").unwrap().is_some());

    batch(|| set_ready.set(true));
    assert!(root.query_selector(".content").unwrap().is_some());
    assert!(root.query_selector(".fallback").unwrap().is_none());
    krab_client::unmount(&root);
}

/// The streaming swap runtime dispatches `krab:suspense-resolved`; after
/// `hydrate()` has installed its listener, islands in the swapped-in nodes are
/// hydrated.
#[wasm_bindgen_test]
fn islands_in_streamed_content_are_hydrated_on_the_resolved_event() {
    // Installs the listener (and hydrates whatever is already mounted).
    let root = support::mount_container(ROOT_ID);
    krab_client::hydrate();

    let doc = support::document();
    let holder = doc.create_element("div").expect("div");
    holder.set_inner_html(concat!(
        r#"<div data-island="Counter" data-props='{"initial":4}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="bst" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button>Count: <span>4</span></button></div>"#
    ));
    let island: Element = holder.first_element_child().expect("island markup");
    root.append_child(&island).expect("append");

    let nodes = js_sys::Array::new();
    nodes.push(&island);
    let detail = js_sys::Object::new();
    js_sys::Reflect::set(&detail, &"id".into(), &"s1".into()).expect("id");
    js_sys::Reflect::set(&detail, &"nodes".into(), &nodes).expect("nodes");
    let event = web_sys::Event::new("krab:suspense-resolved").expect("event");
    js_sys::Reflect::set(&event, &"detail".into(), &detail).expect("detail");
    doc.dispatch_event(&event).expect("dispatch");

    assert_eq!(
        island.get_attribute("data-krab-boundary-state").as_deref(),
        Some("ok"),
        "a streamed-in island must be hydrated"
    );
    krab_client::unmount(&root);
}

/// The swap runtime dispatches `krab:suspense-resolving` with the outgoing
/// fallback nodes before removing them; an island hydrated in the fallback is
/// unmounted, so its closures and effects do not outlive its DOM.
#[wasm_bindgen_test]
fn islands_in_a_fallback_are_unmounted_on_the_resolving_event() {
    let root = support::mount_container(ROOT_ID);
    root.set_inner_html(concat!(
        r#"<div data-island="Counter" data-props='{"initial":1}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="bfb" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button>Count: <span>1</span></button></div>"#
    ));
    krab_client::hydrate();
    let island: Element = root.first_element_child().expect("island");
    assert_eq!(
        island.get_attribute("data-krab-boundary-state").as_deref(),
        Some("ok"),
        "the fallback island must hydrate first"
    );

    let doc = support::document();
    let nodes = js_sys::Array::new();
    nodes.push(&island);
    let detail = js_sys::Object::new();
    js_sys::Reflect::set(&detail, &"id".into(), &"s2".into()).expect("id");
    js_sys::Reflect::set(&detail, &"nodes".into(), &nodes).expect("nodes");
    let event = web_sys::Event::new("krab:suspense-resolving").expect("event");
    js_sys::Reflect::set(&event, &"detail".into(), &detail).expect("detail");
    doc.dispatch_event(&event).expect("dispatch");

    assert_eq!(
        island.get_attribute("data-krab-boundary-state"),
        None,
        "an island in the outgoing fallback must be unmounted"
    );
    krab_client::unmount(&root);
}
