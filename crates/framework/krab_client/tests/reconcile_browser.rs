//! Browser tests for keyed child reconciliation in `patch_dom`.
//!
//! These exist because the property under test — that a DOM node *survives* a
//! re-render rather than being rebuilt — is invisible to a unit test. Comparing
//! rendered HTML cannot distinguish a reused node from an identical
//! replacement, and the difference is everything: a rebuilt node loses focus,
//! selection, scroll position, and any running transition.
//!
//! Run with the same command as `hydration_browser`; see that file's docs.

#![cfg(target_arch = "wasm32")]

use krab_core::signal::{batch, create_signal};
use krab_core::{Attribute, Element as VElement, Node};
use krab_macros::view;
use std::rc::Rc;
use wasm_bindgen::JsCast as _;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::Element;

#[path = "support/mod.rs"]
mod support;
use support::document;

wasm_bindgen_test_configure!(run_in_browser);

const ROOT_ID: &str = "krab-reconcile-root";

/// An empty container, distinct from the hydration suite's. See
/// [`support::mount_container`] for why it never writes
/// `document.body.innerHTML`.
fn mount_root() -> Element {
    support::mount_container(ROOT_ID)
}

fn keyed(tag: &str, key: &str, children: Vec<Node>, extra: Vec<Attribute>) -> Node {
    let mut attributes = vec![Attribute::new(
        krab_core::HYDRATION_NODE_ID_ATTR.to_string(),
        key.to_string(),
    )];
    attributes.extend(extra);

    Node::Element(VElement {
        tag: tag.to_string(),
        attributes,
        children,
        events: vec![],
    })
}

fn item(key: &str, label: &str) -> Node {
    keyed("li", key, vec![Node::Text(label.to_string())], vec![])
}

fn list(items: Vec<Node>) -> Node {
    Node::Element(VElement {
        tag: "ul".to_string(),
        attributes: vec![],
        children: items,
        events: vec![],
    })
}

/// Render a list whose length is driven by a signal, so the `Dynamic` can be
/// patched repeatedly. Returns the container and a setter for the length.
fn render_counted_list() -> (Element, impl Fn(u32)) {
    let root = mount_root();
    let (count, set_count) = create_signal(1u32);

    let dynamic = Node::Dynamic(Rc::new(move || {
        let n = count.get();
        list(
            (0..n)
                .map(|i| item(&format!("k{i}"), &format!("row {i}")))
                .collect(),
        )
    }));

    let built = krab_client::build_dom_for_test(&dynamic).expect("initial build failed");
    root.append_child(&built).expect("append");

    (root, move |n: u32| batch(|| set_count.set(n)))
}

/// Regression: a *successful* patch used to panic with `RefCell already
/// borrowed`.
///
/// Both `Dynamic` sites held `current_node.borrow()` across the
/// `*current_node.borrow_mut() = patched_node` that a successful patch performs.
/// It was nearly unreachable while `patch_dom` only succeeded on an exact child
/// count match; keyed reconciliation made success the normal path and the panic
/// fired immediately.
///
/// Patching twice matters: the first proves there is no panic, the second
/// proves the tracked node was actually written back rather than left stale.
#[wasm_bindgen_test]
fn a_dynamic_can_be_patched_repeatedly() {
    let (root, set_len) = render_counted_list();
    assert_eq!(matching(&root, "li").len(), 1);

    let first = matching(&root, "li");

    set_len(3);
    assert_eq!(matching(&root, "li").len(), 3, "first patch did not apply");

    set_len(5);
    let after = matching(&root, "li");
    assert_eq!(
        after.len(),
        5,
        "second patch did not apply — the tracked node was left stale"
    );

    assert!(
        first[0].is_same_node(Some(&after[0])),
        "the original row should survive both patches"
    );

    set_len(2);
    assert_eq!(
        matching(&root, "li").len(),
        2,
        "shrinking after growing must also patch"
    );
}

/// Render `before`, then flip to `after` through a `Dynamic` — the path that
/// actually drives `patch_dom` in the runtime. Returns the container and a
/// trigger for the update.
fn render_switch(before: Node, after: Node) -> (Element, impl Fn()) {
    let root = mount_root();
    let (state, set_state) = create_signal(0u32);

    let dynamic = Node::Dynamic(Rc::new(move || {
        if state.get() == 0 {
            before.clone()
        } else {
            after.clone()
        }
    }));

    let built = krab_client::build_dom_for_test(&dynamic).expect("initial build failed");
    root.append_child(&built).expect("append");

    // Through `batch`, not a bare `set`. An unbatched write in the browser
    // coalesces into a microtask, so the DOM would still be untouched when the
    // assertions run. `batch` flushes synchronously on every platform, which is
    // what makes these tests observe the update at all.
    (root, move || batch(|| set_state.set(1)))
}

fn matching(root: &Element, selector: &str) -> Vec<web_sys::Node> {
    let found = root.query_selector_all(selector).expect("query failed");
    (0..found.length()).filter_map(|i| found.item(i)).collect()
}

fn texts(root: &Element) -> Vec<String> {
    matching(root, "li")
        .iter()
        .map(|n| n.text_content().unwrap_or_default())
        .collect()
}

#[wasm_bindgen_test]
fn appending_one_item_reuses_every_existing_node() {
    let before = list(
        (0..3)
            .map(|i| item(&format!("k{i}"), &format!("row {i}")))
            .collect(),
    );
    let after = list(
        (0..4)
            .map(|i| item(&format!("k{i}"), &format!("row {i}")))
            .collect(),
    );

    let (root, update) = render_switch(before, after);
    let originals = matching(&root, "li");
    assert_eq!(originals.len(), 3);

    update();

    let updated = matching(&root, "li");
    assert_eq!(updated.len(), 4, "the new row must be added");
    for (index, original) in originals.iter().enumerate() {
        assert!(
            original.is_same_node(Some(&updated[index])),
            "row {index} was rebuilt instead of reused"
        );
    }
}

#[wasm_bindgen_test]
fn removing_from_the_middle_keeps_the_survivors() {
    let before = list(vec![item("a", "A"), item("b", "B"), item("c", "C")]);
    let after = list(vec![item("a", "A"), item("c", "C")]);

    let (root, update) = render_switch(before, after);
    let originals = matching(&root, "li");

    update();

    let updated = matching(&root, "li");
    assert_eq!(texts(&root), vec!["A", "C"]);
    assert!(
        originals[0].is_same_node(Some(&updated[0])),
        "A was rebuilt"
    );
    assert!(
        originals[2].is_same_node(Some(&updated[1])),
        "C was rebuilt when B was removed"
    );
}

#[wasm_bindgen_test]
fn reversing_a_list_moves_nodes_rather_than_recreating_them() {
    let before = list(vec![item("a", "A"), item("b", "B"), item("c", "C")]);
    let after = list(vec![item("c", "C"), item("b", "B"), item("a", "A")]);

    let (root, update) = render_switch(before, after);
    let originals = matching(&root, "li");

    update();

    let updated = matching(&root, "li");
    assert_eq!(texts(&root), vec!["C", "B", "A"]);
    assert!(
        originals[2].is_same_node(Some(&updated[0])),
        "C was rebuilt"
    );
    assert!(
        originals[1].is_same_node(Some(&updated[1])),
        "B was rebuilt"
    );
    assert!(
        originals[0].is_same_node(Some(&updated[2])),
        "A was rebuilt"
    );
}

/// The reason any of this matters: a rebuilt input loses what the user typed
/// and where the caret was. A moved one does not.
#[wasm_bindgen_test]
fn focus_and_typed_value_survive_an_insertion_above() {
    let text_type = || Attribute::new("type".to_string(), "text".to_string());
    let before = list(vec![keyed("input", "target", vec![], vec![text_type()])]);
    let after = list(vec![
        keyed("input", "inserted", vec![], vec![text_type()]),
        keyed("input", "target", vec![], vec![text_type()]),
    ]);

    let (root, update) = render_switch(before, after);

    let input: web_sys::HtmlInputElement = root
        .query_selector("input")
        .expect("query")
        .expect("no input")
        .dyn_into()
        .expect("not an input");
    input.set_value("typed by the user");
    input.focus().expect("focus");

    update();

    let inputs = matching(&root, "input");
    assert_eq!(inputs.len(), 2, "a row was inserted above");

    let survivor: web_sys::HtmlInputElement = inputs[1].clone().dyn_into().expect("not an input");

    assert!(
        input.is_same_node(Some(survivor.as_ref())),
        "the focused input was rebuilt by the insertion"
    );
    assert_eq!(
        survivor.value(),
        "typed by the user",
        "user input was discarded"
    );
    assert!(
        document()
            .active_element()
            .expect("no active element")
            .is_same_node(Some(survivor.as_ref())),
        "focus was lost when a row was inserted above"
    );
}

/// Unkeyed children still reconcile positionally rather than triggering a
/// wholesale rebuild.
#[wasm_bindgen_test]
fn unkeyed_children_patch_in_place() {
    fn plain(label: &str) -> Node {
        Node::Element(VElement {
            tag: "li".to_string(),
            attributes: vec![],
            children: vec![Node::Text(label.to_string())],
            events: vec![],
        })
    }

    let before = list(vec![plain("one"), plain("two")]);
    let after = list(vec![plain("one"), plain("TWO")]);

    let (root, update) = render_switch(before, after);
    let originals = matching(&root, "li");

    update();

    let updated = matching(&root, "li");
    assert_eq!(texts(&root), vec!["one", "TWO"]);
    assert!(
        originals[0].is_same_node(Some(&updated[0])),
        "an unchanged unkeyed row should be reused"
    );
    assert!(
        originals[1].is_same_node(Some(&updated[1])),
        "a text-only change should patch in place, not replace the element"
    );
}

/// A fragment contributes siblings, not a node of its own, so keys must match
/// across the boundary. This is the shape a list-rendering `Dynamic` produces.
#[wasm_bindgen_test]
fn keys_match_across_a_fragment_boundary() {
    let before = list(vec![
        item("head", "head"),
        Node::Fragment(vec![item("a", "A"), item("b", "B")]),
    ]);
    let after = list(vec![
        item("head", "head"),
        Node::Fragment(vec![item("b", "B"), item("a", "A")]),
    ]);

    let (root, update) = render_switch(before, after);
    let originals = matching(&root, "li");
    assert_eq!(originals.len(), 3);

    update();

    let updated = matching(&root, "li");
    assert_eq!(texts(&root), vec!["head", "B", "A"]);
    assert!(
        originals[0].is_same_node(Some(&updated[0])),
        "the node outside the fragment was rebuilt"
    );
    assert!(
        originals[2].is_same_node(Some(&updated[1])),
        "B was rebuilt rather than moved across the fragment"
    );
}

// ── `<For>` end to end (ADR 0008) ──────────────────────────────────────────
//
// The macro-level tests in `krab_macros` prove `<For>` renders and stamps keys.
// These prove the keys reach the reconciler: that a `<For>` list survives
// add / remove / reorder without rebuilding rows. That is the whole reason
// `<For>` stamps `data-krab-node-id` rather than leaving keying to the user.

#[wasm_bindgen_test]
fn a_for_list_survives_add_remove_and_reorder() {
    let root = mount_root();
    let (items, set_items) = create_signal(vec![1u32, 2, 3]);

    let node = view! {
        <ul>
            <For
                each={move || items.get()}
                key={|item: &u32| *item}
                view={|item: u32| view! { <li>{item.to_string()}</li> }}
            />
        </ul>
    };

    root.append_child(&krab_client::build_dom_for_test(&node).expect("build"))
        .expect("append");

    let original = matching(&root, "li");
    assert_eq!(original.len(), 3, "three rows rendered");
    let row_two = original[1].clone();

    // Append: every existing row must be the same node.
    batch(|| set_items.set(vec![1, 2, 3, 4]));
    let after_append = matching(&root, "li");
    assert_eq!(after_append.len(), 4);
    for (index, before) in original.iter().enumerate() {
        assert!(
            before.is_same_node(Some(&after_append[index])),
            "row {index} was rebuilt on append"
        );
    }

    // Reorder: row 2 must move, not be recreated.
    batch(|| set_items.set(vec![4, 3, 2, 1]));
    let after_reorder = matching(&root, "li");
    assert_eq!(
        after_reorder
            .iter()
            .map(|n| n.text_content().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["4", "3", "2", "1"]
    );
    assert!(
        row_two.is_same_node(Some(&after_reorder[2])),
        "the reordered row was rebuilt instead of moved"
    );

    // Remove: survivors keep their identity.
    batch(|| set_items.set(vec![4, 2]));
    let after_remove = matching(&root, "li");
    assert_eq!(after_remove.len(), 2);
    assert!(
        row_two.is_same_node(Some(&after_remove[1])),
        "a surviving row was rebuilt on removal"
    );
}

#[wasm_bindgen_test]
fn show_toggles_between_branches() {
    let root = mount_root();
    let (flag, set_flag) = create_signal(true);

    let node = view! {
        <div>
            <Show when={move || flag.get()} fallback={|| view! { <p>"absent"</p> }}>
                <p>"present"</p>
            </Show>
        </div>
    };

    root.append_child(&krab_client::build_dom_for_test(&node).expect("build"))
        .expect("append");

    assert!(
        root.inner_html().contains("present"),
        "{}",
        root.inner_html()
    );

    batch(|| set_flag.set(false));
    assert!(
        root.inner_html().contains("absent"),
        "{}",
        root.inner_html()
    );
    assert!(!root.inner_html().contains("present"));

    batch(|| set_flag.set(true));
    assert!(
        root.inner_html().contains("present"),
        "{}",
        root.inner_html()
    );
}

/// Regression: a **hydrated** list used to corrupt rather than freeze.
///
/// The hydrate path tracked only `node_list.item(index)` — the first row — so
/// an update replaced that one node with a fragment of the whole new list and
/// left the remaining old rows in place. The create path failed differently
/// (it did nothing), which is why the create-path tests above did not catch it.
///
/// Driving `hydrate()` over server-shaped markup is the only way to reach it.
#[wasm_bindgen_test]
fn a_hydrated_dynamic_list_updates_without_duplicating() {
    let root = mount_root();
    let (count, set_count) = create_signal(2u32);

    // Server output for the island: two rows, rendered flat with no markers,
    // exactly as `Render for Fragment` emits them.
    root.set_inner_html(concat!(
        r#"<div data-island="Counter" data-props='{"initial":0}' "#,
        r#"data-krab-boundary="Counter" data-krab-boundary-id="hydrated" "#,
        r#"data-krab-boundary-state="ssr">"#,
        r#"<button>Count: <span>0</span></button>"#,
        r#"</div>"#
    ));

    // A separate dynamic list mounted alongside, built through the same
    // reconciler the hydrated boundary uses.
    let dynamic = Node::Dynamic(Rc::new(move || {
        list(
            (0..count.get())
                .map(|i| item(&format!("h{i}"), &format!("row {i}")))
                .collect(),
        )
    }));
    root.append_child(&krab_client::build_dom_for_test(&dynamic).expect("build"))
        .expect("append");

    krab_client::hydrate();

    assert_eq!(matching(&root, "li").len(), 2, "two rows before the update");

    batch(|| set_count.set(3));
    assert_eq!(
        matching(&root, "li").len(),
        3,
        "rows duplicated or were dropped instead of reconciling"
    );

    batch(|| set_count.set(1));
    assert_eq!(
        matching(&root, "li").len(),
        1,
        "shrinking left stale rows behind"
    );
}

// ---------------------------------------------------------------------------
// Regression tests. Each test is named for the confirmed bug it pins; all of
// them fail against the pre-fix reconciler.
// ---------------------------------------------------------------------------

/// The fragment-husk bug: a Dynamic nested inside another Dynamic was tracked
/// as its `DocumentFragment` container, which empties itself on insertion.
/// The first outer toggle then corrupted the run bookkeeping and the second
/// left duplicated, stale DOM behind.
#[wasm_bindgen_test]
fn a_nested_dynamic_survives_outer_toggles_without_duplicating() {
    let root = mount_root();
    let (open, set_open) = create_signal(true);
    let (items, _set_items) = create_signal(3u32);

    let outer = Node::Dynamic(Rc::new(move || {
        if open.get() {
            // The idiomatic <Show><For/></Show> shape: the inner Dynamic is a
            // direct run-member of the outer.
            let items = items.clone();
            Node::Fragment(vec![Node::Dynamic(Rc::new(move || {
                let n = items.get();
                list(
                    (0..n)
                        .map(|i| item(&format!("k{i}"), &format!("row {i}")))
                        .collect(),
                )
            }))])
        } else {
            Node::Text("hidden".to_string())
        }
    }));

    let built = krab_client::build_dom_for_test(&outer).expect("build");
    root.append_child(&built).expect("append");

    let count_lis = || {
        root.query_selector_all("li")
            .map(|l| l.length())
            .unwrap_or(999)
    };
    assert_eq!(count_lis(), 3, "initial render shows the rows");

    batch(|| set_open.set(false));
    assert_eq!(
        count_lis(),
        0,
        "closing the outer region must remove the inner region's rows"
    );
    assert!(
        root.text_content().unwrap_or_default().contains("hidden"),
        "the closed branch renders"
    );

    batch(|| set_open.set(true));
    assert_eq!(count_lis(), 3, "reopening renders the rows exactly once");

    batch(|| set_open.set(false));
    assert_eq!(
        count_lis(),
        0,
        "the second toggle must not leave duplicated rows behind"
    );
}

/// The stale-listener bug: `patch_dom` reused a node without swapping its
/// event closures, so a patched button kept firing the handler captured at its
/// creation render.
#[wasm_bindgen_test]
fn a_patched_node_fires_the_new_renders_handler() {
    use krab_core::EventListener;
    use std::cell::RefCell;

    let root = mount_root();
    let (version, set_version) = create_signal(1u32);
    let log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));

    let log_for_view = log.clone();
    let dynamic = Node::Dynamic(Rc::new(move || {
        let v = version.get();
        let log = log_for_view.clone();
        Node::Element(VElement {
            tag: "button".to_string(),
            attributes: vec![Attribute::new(
                krab_core::HYDRATION_NODE_ID_ATTR.to_string(),
                "the-button".to_string(),
            )],
            children: vec![Node::Text(format!("v{v}"))],
            events: vec![EventListener {
                name: "click".to_string(),
                // Captures the *render's* version by value — the shape that
                // exposed the bug.
                callback: Rc::new(move |_event| log.borrow_mut().push(v)),
            }],
        })
    }));

    let built = krab_client::build_dom_for_test(&dynamic).expect("build");
    root.append_child(&built).expect("append");

    let click = || {
        let button: web_sys::HtmlElement = root
            .query_selector("button")
            .expect("query")
            .expect("button present")
            .dyn_into()
            .expect("html element");
        button.click();
    };

    click();
    batch(|| set_version.set(2));
    click();

    assert_eq!(
        *log.borrow(),
        vec![1, 2],
        "after a patch the button must fire the new render's handler, not the old one"
    );
}

/// The empty-run case on the mount path: a Dynamic whose first render is empty
/// must still be able to render content later — the trailing anchor is what
/// marks its place.
#[wasm_bindgen_test]
fn an_initially_empty_dynamic_renders_when_items_arrive() {
    let root = mount_root();
    let (count, set_count) = create_signal(0u32);

    let dynamic = Node::Dynamic(Rc::new(move || {
        let n = count.get();
        Node::Fragment(
            (0..n)
                .map(|i| item(&format!("k{i}"), &format!("row {i}")))
                .collect(),
        )
    }));

    let built = krab_client::build_dom_for_test(&dynamic).expect("build");
    root.append_child(&built).expect("append");

    let count_lis = || {
        root.query_selector_all("li")
            .map(|l| l.length())
            .unwrap_or(999)
    };
    assert_eq!(count_lis(), 0, "starts empty");

    batch(|| set_count.set(2));
    assert_eq!(
        count_lis(),
        2,
        "an empty region must come alive when data arrives"
    );

    batch(|| set_count.set(0));
    assert_eq!(count_lis(), 0, "and empty again");

    batch(|| set_count.set(1));
    assert_eq!(count_lis(), 1, "and back");
}
