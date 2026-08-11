//! Browser tests for the hydration runtime.
//!
//! Run with:
//!
//! ```sh
//! cargo install wasm-bindgen-cli --version <the version in Cargo.lock>
//! CHROMEDRIVER=/path/to/chromedriver \
//!   cargo test -p krab_client --target wasm32-unknown-unknown --features web
//! ```
//!
//! [`.cargo/config.toml`](../../../../.cargo/config.toml) routes the wasm32
//! target through `wasm-bindgen-test-runner`, which drives a real browser.
//!
//! `wasm-pack test` also works but is not the documented path: it bundles its
//! own `wasm-bindgen` and generates the JS shim itself, so two versions end up
//! in play. Going through cargo keeps exactly the one Cargo resolved.
//!
//! # Requires wasm-bindgen >= 0.2.127 against a current Chrome
//!
//! 0.2.114 cannot open a session against ChromeDriver 151 — it fails parsing
//! the `newSession` response (`invalid type: map, expected a string`) before
//! any test body runs. The workspace is pinned above that.
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
use web_sys::Element;

#[path = "support/mod.rs"]
mod support;
use support::document;

wasm_bindgen_test_configure!(run_in_browser);

/// Id of the container every test mounts into.
const TEST_ROOT_ID: &str = "krab-test-root";

/// Mount `markup` into a dedicated container and return the first island in it.
///
/// The container comes from [`support::mount_container`] — see the warning
/// there about never writing `document.body.innerHTML`. Each test gets a
/// freshly emptied container, because `hydrate()` scans the whole document for
/// `[data-island]` and would otherwise re-hydrate leftovers from the previous
/// test.
fn mount(markup: &str) -> Element {
    let root = support::mount_container(TEST_ROOT_ID);
    root.set_inner_html(markup);
    root.query_selector("[data-island]")
        .expect("query failed")
        .expect("no island element in mounted markup")
}

/// The mounted container, for tests that need to look at more than one island.
fn test_root() -> Element {
    document()
        .query_selector(&format!("#{TEST_ROOT_ID}"))
        .expect("query failed")
        .expect("test root not mounted")
}

fn attr(element: &Element, name: &str) -> Option<String> {
    element.get_attribute(name)
}

/// Server-rendered markup for the `Counter` island shipped in this crate.
///
/// Mirrors what `#[island]` emits: the wrapper attributes plus the SSR content
/// the island would have produced for these props.
/// **No whitespace between elements.** `krab_core::Render` concatenates
/// children with no separator, so real SSR output contains no whitespace-only
/// text nodes. Indented fixture markup makes the browser create them, and
/// hydration then reports `expected <button> but found #text(...)` — a fixture
/// artefact that looks exactly like a runtime defect. Whitespace *inside* a
/// start tag is fine; it never becomes a node.
fn counter_ssr(initial: i32) -> String {
    format!(
        concat!(
            r#"<div data-island="Counter" data-props='{{"initial":{0}}}' "#,
            r#"data-krab-boundary="Counter" data-krab-boundary-id="b1" "#,
            r#"data-krab-boundary-state="ssr">"#,
            r#"<button>Count: <span>{0}</span></button>"#,
            r#"</div>"#
        ),
        initial
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
    // `classify_boundary_state`: "ok" | "patched" | "error" | "decode-error".
    assert_eq!(
        attr(&island, "data-krab-boundary-state").as_deref(),
        Some("ok"),
        "a clean hydration must report itself as ok"
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
    let markup = concat!(
        r#"<div data-island="Counter" data-props='{"initial":0}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="b1" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button>Count: <span>7</span></button>"#,
        r#"</div>"#
    );
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
    let markup = concat!(
        r#"<div data-island="NoSuchIsland" data-props='{}' "#,
        r#"data-krab-boundary="NoSuchIsland" data-krab-boundary-id="b1" "#,
        r#"data-krab-boundary-state="ssr"></div>"#
    );
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
        concat!(
            r#"<div data-island="Counter" data-props='not json' "#,
            r#"data-krab-boundary="Counter" data-krab-boundary-id="bad" "#,
            r#"data-krab-boundary-state="ssr"><button></button></div>{}"#
        ),
        counter_ssr(0)
    );

    // Via `mount`, not the body — see the note on `mount`.
    mount(&markup);

    krab_client::hydrate();

    let islands = test_root()
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
    let markup = concat!(
        r#"<div data-island="Counter" data-props='{"initial":0}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="b1" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button>Count: <span>0</span><span>stale</span></button>"#,
        r#"</div>"#
    );
    let island = mount(markup);

    krab_client::hydrate();

    assert!(
        !island.inner_html().contains("stale"),
        "an extra server-rendered child must be removed; DOM was: {}",
        island.inner_html()
    );
}

use wasm_bindgen::JsCast as _;

/// SSR markup for the `Toggle` island, whose two element children make it the
/// only shipped island that can express a reorder.
///
/// The `data-krab-node-id` values are predictable: `hydrate` annotates the
/// client tree with the boundary id read from the DOM, root path `0`, children
/// `0.0` and `0.1` — see `HydrationNodeMarker::as_attr_value`.
fn toggle_ssr(children_swapped: bool) -> String {
    let button = r#"<button data-krab-node-id="b1/0.0">Toggle</button>"#;
    let span = r#"<span data-krab-node-id="b1/0.1"> OFF</span>"#;
    let inner = if children_swapped {
        format!("{span}{button}")
    } else {
        format!("{button}{span}")
    };

    format!(
        concat!(
            r#"<div data-island="Toggle" data-props='{{"initial":false}}' "#,
            r#"data-krab-boundary="Toggle" data-krab-boundary-id="b1" "#,
            r#"data-krab-boundary-state="ssr">"#,
            r#"<div data-krab-node-id="b1/0">{}</div>"#,
            r#"</div>"#
        ),
        inner
    )
}

/// Ported from the deleted `#[cfg(test)]` planning model, which asserted this
/// against a reimplementation rather than the runtime.
///
/// Children that carry hydration markers must be **moved** into the expected
/// order, not torn down and rebuilt — rebuilding would lose focus and any
/// attached listener on a node the user is interacting with.
#[wasm_bindgen_test]
fn marker_matched_children_in_the_wrong_order_are_moved_not_rebuilt() {
    let island = mount(&toggle_ssr(true));

    let span_before = island
        .query_selector("span")
        .expect("query failed")
        .expect("no span in SSR markup");

    krab_client::hydrate();

    let children = island
        .query_selector("div[data-krab-node-id]")
        .expect("query failed")
        .expect("no inner wrapper");

    let first = children
        .first_element_child()
        .expect("wrapper has no children");
    assert_eq!(
        first.tag_name().to_lowercase(),
        "button",
        "the button must end up first, matching the client render; DOM was: {}",
        island.inner_html()
    );

    let span_after = island
        .query_selector("span")
        .expect("query failed")
        .expect("span vanished during hydration");
    assert!(
        span_before.is_same_node(Some(span_after.as_ref())),
        "a reordered node must be moved, not replaced"
    );
}

/// Ported from the deleted planning model.
///
/// Attributes are the client's to own after hydration; a server/client
/// attribute difference must not cause the element to be torn down.
#[wasm_bindgen_test]
fn an_attribute_difference_alone_does_not_replace_the_node() {
    let markup = concat!(
        r#"<div data-island="Counter" data-props='{"initial":0}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="b1" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button class="stale-server-class">Count: <span>0</span></button>"#,
        r#"</div>"#
    );
    let island = mount(markup);
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
        "an attribute difference must not cause a replacement"
    );
}

// ---------------------------------------------------------------------------
// Regression tests, hydration path.
// ---------------------------------------------------------------------------

use krab_core::signal::{batch, create_signal, ReadSignal, WriteSignal};
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static EMPTY_LIST_COUNT: RefCell<Option<(ReadSignal<u32>, WriteSignal<u32>)>> =
        const { RefCell::new(None) };
}

fn empty_list_count() -> (ReadSignal<u32>, WriteSignal<u32>) {
    EMPTY_LIST_COUNT.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| create_signal(0))
            .clone()
    })
}

/// A test island whose whole body is a `Dynamic` that server-rendered empty.
fn empty_list_factory(_props: String) -> krab_core::Node {
    let (count, _set) = empty_list_count();
    krab_core::Node::Element(krab_core::Element {
        tag: "ul".to_string(),
        attributes: vec![],
        children: vec![krab_core::Node::Dynamic(Rc::new(move || {
            let n = count.get();
            krab_core::Node::Fragment(
                (0..n)
                    .map(|i| {
                        krab_core::Node::Element(krab_core::Element {
                            tag: "li".to_string(),
                            attributes: vec![krab_core::Attribute::new(
                                krab_core::HYDRATION_NODE_ID_ATTR.to_string(),
                                format!("row-{i}"),
                            )],
                            children: vec![krab_core::Node::Text(format!("row {i}"))],
                            events: vec![],
                        })
                    })
                    .collect(),
            )
        }))],
        events: vec![],
    })
}

inventory::submit! {
    krab_client::IslandDefinition { name: "EmptyList", factory: empty_list_factory }
}

/// The dead-region bug: a hydrated `Dynamic` whose SSR output was empty (a
/// `<For>` over an empty list, `<Show when=false>`) could never create its
/// anchor, so every later update was silently dropped and content never
/// appeared for the life of the page.
#[wasm_bindgen_test]
fn an_initially_empty_hydrated_dynamic_can_render_later() {
    let (_count, set_count) = empty_list_count();
    batch(|| set_count.set(0));

    let island = mount(concat!(
        r#"<div data-island="EmptyList" data-props='{}' "#,
        r#"data-krab-boundary="EmptyList" data-krab-boundary-id="be" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<ul data-krab-node-id="be/0"></ul>"#,
        r#"</div>"#
    ));
    krab_client::hydrate();

    let count_lis = || {
        island
            .query_selector_all("li")
            .map(|l| l.length())
            .unwrap_or(999)
    };
    assert_eq!(count_lis(), 0, "hydrates empty, as the server rendered");

    batch(|| set_count.set(2));
    assert_eq!(
        count_lis(),
        2,
        "an empty-SSR region must come alive when data arrives, not stay dead forever"
    );

    batch(|| set_count.set(1));
    assert_eq!(count_lis(), 1, "and keeps reconciling afterwards");
}

/// The silent-mismatch gap (pre-existing on main): a node whose hydration
/// marker disagrees with the expected path but whose tag matches was reused
/// with zero counted mismatches, so the boundary reported a clean `ok` over a
/// mis-wired subtree. The reuse is kept; the accounting is fixed.
#[wasm_bindgen_test]
fn a_marker_mismatch_is_counted_and_reported_as_patched() {
    let island = mount(concat!(
        r#"<div data-island="Toggle" data-props='{"initial":false}' "#,
        r#"data-krab-boundary="Toggle" data-krab-boundary-id="b1" "#,
        r#"data-krab-boundary-state="ssr">"#,
        // Wrong marker: the client expects b1/0, and no sibling carries it.
        r#"<div data-krab-node-id="b1/9.9">"#,
        r#"<button data-krab-node-id="b1/0.0">Toggle</button>"#,
        r#"<span data-krab-node-id="b1/0.1"> OFF</span>"#,
        r#"</div>"#,
        r#"</div>"#
    ));
    krab_client::hydrate();

    let mismatches: u32 = attr(&island, "data-krab-boundary-mismatches")
        .unwrap_or_default()
        .parse()
        .unwrap_or(0);
    assert!(
        mismatches > 0,
        "a reused marker-mismatched node must be counted, not silent"
    );
    assert_eq!(
        attr(&island, "data-krab-boundary-state").as_deref(),
        Some("patched"),
        "the boundary must not report a clean 'ok' over a mis-wired subtree"
    );
}
