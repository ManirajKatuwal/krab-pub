//! Browser tests for the hydration runtime.
//!
//! Run with:
//!
//! ```sh
//! wasm-pack test --headless --chrome crates/framework/krab_client -- --features web
//! ```
//!
//! # Why these exist separately
//!
//! `cargo test --workspace` compiles this crate for the host, where there is no
//! `document` — so it can only ever reach the pure planning functions in
//! `lib.rs`. Everything that actually mutates the DOM (`hydrate`,
//! `hydrate_recursive`, `create_dom_node`, `patch_dom`) was reachable by no test
//! at all. A hydration defect's failure mode is a subtly wrong DOM in a user's
//! browser, not a red build, which is the worst combination of high blast
//! radius and low detectability in the framework.
//!
//! These assert the boundary contract that `hydrate()` publishes back onto the
//! DOM: `data-krab-boundary-state` and `data-krab-boundary-mismatches`. That is
//! the seam the runtime already uses to report itself, so tests written against
//! it stay valid across refactors of the internals.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Document, Element};

wasm_bindgen_test_configure!(run_in_browser);

fn document() -> Document {
    web_sys::window()
        .expect("no window")
        .document()
        .expect("no document")
}

/// Replace the body with `markup` and return the first island element.
///
/// Each test gets a clean body: `hydrate()` scans the whole document for
/// `[data-island]`, so leftovers from a previous test would be hydrated again.
fn mount(markup: &str) -> Element {
    let doc = document();
    let body = doc.body().expect("no body");
    body.set_inner_html(markup);
    doc.query_selector("[data-island]")
        .expect("query failed")
        .expect("no island element in mounted markup")
}

fn attr(element: &Element, name: &str) -> Option<String> {
    element.get_attribute(name)
}

/// Server-rendered markup for the `Counter` island shipped in this crate.
///
/// Mirrors what `#[island]` emits: the wrapper attributes plus the SSR content
/// the island would have produced for these props.
fn counter_ssr(initial: i32) -> String {
    format!(
        r#"<div data-island="Counter"
                 data-props='{{"initial":{initial}}}'
                 data-krab-boundary="Counter"
                 data-krab-boundary-id="b1"
                 data-krab-boundary-state="ssr">
             <button>Count: <span>{initial}</span></button>
           </div>"#
    )
}

#[wasm_bindgen_test]
fn hydrating_matching_markup_reports_no_mismatches() {
    let island = mount(&counter_ssr(0));
    krab_client::hydrate();

    assert_eq!(
        attr(&island, "data-krab-boundary-mismatches").as_deref(),
        Some("0"),
        "server markup matching the client render must patch nothing"
    );
    assert_eq!(
        attr(&island, "data-krab-boundary-state").as_deref(),
        Some("hydrated"),
        "a clean hydration must report itself as hydrated"
    );
}

#[wasm_bindgen_test]
fn hydration_preserves_the_server_rendered_dom_node() {
    // Reuse, not replace, is the whole point of hydration: replacing the node
    // would discard focus, scroll position, and any in-flight CSS transition.
    let island = mount(&counter_ssr(0));
    let button_before = island
        .query_selector("button")
        .expect("query failed")
        .expect("no button in SSR markup");

    krab_client::hydrate();

    let button_after = island
        .query_selector("button")
        .expect("query failed")
        .expect("button vanished during hydration");

    assert!(
        button_before.is_same_node(Some(button_after.as_ref())),
        "hydration replaced the server-rendered node instead of reusing it"
    );
}

#[wasm_bindgen_test]
fn a_client_server_content_mismatch_is_patched_and_counted() {
    // The server said 7; the props say 0, so the client renders 0. The runtime
    // must correct the DOM and say that it did.
    let markup = r#"<div data-island="Counter"
                         data-props='{"initial":0}'
                         data-krab-boundary="Counter"
                         data-krab-boundary-id="b1"
                         data-krab-boundary-state="ssr">
                      <button>Count: <span>7</span></button>
                    </div>"#;
    let island = mount(markup);

    krab_client::hydrate();

    let mismatches: u32 = attr(&island, "data-krab-boundary-mismatches")
        .expect("no mismatch count published")
        .parse()
        .expect("mismatch count is not a number");

    assert!(
        mismatches > 0,
        "a server/client content difference must be reported, not silently accepted"
    );
    assert!(
        island.inner_html().contains('0'),
        "the client value must win after hydration; DOM was: {}",
        island.inner_html()
    );
}

#[wasm_bindgen_test]
fn an_unregistered_island_is_reported_not_ignored() {
    let markup = r#"<div data-island="NoSuchIsland"
                         data-props='{}'
                         data-krab-boundary="NoSuchIsland"
                         data-krab-boundary-id="b1"
                         data-krab-boundary-state="ssr"></div>"#;
    let island = mount(markup);

    krab_client::hydrate();

    assert_eq!(
        attr(&island, "data-krab-boundary-state").as_deref(),
        Some("missing-definition"),
        "an island with no registered factory must say so rather than stay 'ssr'"
    );
}

#[wasm_bindgen_test]
fn malformed_props_do_not_abort_hydration_of_other_islands() {
    // One bad island must not take the page down: the second must still hydrate.
    let markup = format!(
        r#"<div data-island="Counter"
                data-props='not json'
                data-krab-boundary="Counter"
                data-krab-boundary-id="bad"
                data-krab-boundary-state="ssr"><button></button></div>
           {}"#,
        counter_ssr(0)
    );

    let doc = document();
    doc.body().expect("no body").set_inner_html(&markup);

    krab_client::hydrate();

    let islands = doc
        .query_selector_all("[data-island]")
        .expect("query failed");
    assert_eq!(islands.length(), 2, "test markup should mount two islands");

    let second: Element = islands
        .item(1)
        .expect("no second island")
        .dyn_into()
        .expect("not an element");

    assert_ne!(
        attr(&second, "data-krab-boundary-state").as_deref(),
        Some("ssr"),
        "a malformed island must not prevent later islands from hydrating"
    );
}

#[wasm_bindgen_test]
fn hydration_is_idempotent() {
    // The runtime can be invoked twice (e.g. a re-entrant bootstrap). The second
    // pass must find everything matching and change nothing.
    let island = mount(&counter_ssr(3));

    krab_client::hydrate();
    let after_first = island.inner_html();

    krab_client::hydrate();
    let after_second = island.inner_html();

    assert_eq!(
        after_first, after_second,
        "a second hydration pass must be a no-op"
    );
    assert_eq!(
        attr(&island, "data-krab-boundary-mismatches").as_deref(),
        Some("0"),
        "the second pass must not report spurious mismatches"
    );
}

#[wasm_bindgen_test]
fn an_island_with_extra_server_children_has_them_removed() {
    // Stale markup from an older release: the client render has one span, the
    // server emitted two. The extra must go, and be counted.
    let markup = r#"<div data-island="Counter"
                         data-props='{"initial":0}'
                         data-krab-boundary="Counter"
                         data-krab-boundary-id="b1"
                         data-krab-boundary-state="ssr">
                      <button>Count: <span>0</span><span>stale</span></button>
                    </div>"#;
    let island = mount(markup);

    krab_client::hydrate();

    assert!(
        !island.inner_html().contains("stale"),
        "an extra server-rendered child must be removed; DOM was: {}",
        island.inner_html()
    );
}

use wasm_bindgen::JsCast as _;
