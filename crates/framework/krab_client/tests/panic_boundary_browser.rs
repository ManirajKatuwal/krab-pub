//! Browser tests for per-island panic isolation in [`krab_client::hydrate`].
//!
//! Run with:
//!
//! ```sh
//! CHROMEDRIVER=/path/to/chromedriver \
//!   cargo test -p krab_client --target wasm32-unknown-unknown --features web
//! ```
//!
//! # What is under test
//!
//! `wasm32-unknown-unknown` is `panic = "abort"`: a panic inside an island
//! factory runs the panic hook and then executes `unreachable`, trapping. Rust
//! cannot catch that — `catch_unwind` never returns `Err` on this target — and
//! until 0.6.0 the trap escaped the whole hydration loop, leaving every island
//! after the panicking one at `data-krab-boundary-state="ssr"`, inert, with no
//! diagnostic. These tests used to be characterization tests of that failure.
//!
//! Since 0.6.0 each island is hydrated through a JS `try`/`catch` trampoline
//! (`src/isolation.rs`). The trap unwinds only to the trampoline; the loop
//! below it stamps the failing boundary `error`, renders the `role="alert"`
//! fallback, and continues. These tests assert that contract:
//!
//! - no trap escapes `hydrate()` to its caller;
//! - the panicking boundary ends in `error` with the fallback markup;
//! - islands before *and after* it hydrate to `ok`;
//! - the scoped exports (`hydrate_island`, `hydrate_within_selector`) are
//!   isolated the same way and report the failure to their caller.
//!
//! # How an escaping trap would be observed without killing the runner
//!
//! A wasm trap surfaces at the JS boundary as a `RuntimeError`. Calling
//! `hydrate()` straight from a test body would let a regression escape into the
//! harness as an opaque failure that says nothing about the DOM. The call is
//! therefore routed through a two-line JS trampoline of its own, so that if
//! isolation ever regresses the test survives the trap and fails with an
//! assertion that names it.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{console, Element};

#[path = "support/mod.rs"]
mod support;
// `Counter` and `Toggle`, which `krab_client` itself shipped until 0.6.0.
#[path = "support/islands.rs"]
mod islands;
use support::document;

wasm_bindgen_test_configure!(run_in_browser);

/// This suite's own container, kept distinct from the other suites'.
const TEST_ROOT_ID: &str = "krab-panic-boundary-root";

/// The panic message, asserted on in the diagnostic path.
const PANIC_MESSAGE: &str = "intentional island factory panic from panic_boundary_browser.rs";

/// An island whose factory panics the moment hydration calls it.
fn panicking_factory(_props: String) -> krab_core::Node {
    panic!("{}", PANIC_MESSAGE);
}

inventory::submit! {
    krab_client::IslandDefinition { name: "PanicIsland", factory: panicking_factory }
}

/// Call `f` from JavaScript inside a `try`/`catch`.
///
/// Returns `None` if `f` returned normally, or `Some(description)` if it
/// trapped. With isolation working, every call in this suite returns `None`.
fn call_catching_traps<F: FnMut() + 'static>(f: F) -> Option<String> {
    let closure = Closure::<dyn FnMut()>::new(f);
    let trampoline = js_sys::Function::new_with_args(
        "f",
        "try { f(); return null; } catch (e) { return String(e); }",
    );
    let outcome = trampoline
        .call1(&JsValue::NULL, closure.as_ref())
        .expect("the JS trampoline itself threw");
    outcome.as_string()
}

fn mount(markup: &str) -> Element {
    let root = support::mount_container(TEST_ROOT_ID);
    root.set_inner_html(markup);
    root
}

fn test_root() -> Element {
    document()
        .query_selector(&format!("#{TEST_ROOT_ID}"))
        .expect("query failed")
        .expect("test root not mounted")
}

fn island(boundary_id: &str) -> Element {
    test_root()
        .query_selector(&format!("[data-krab-boundary-id='{boundary_id}']"))
        .expect("query failed")
        .unwrap_or_else(|| panic!("no island with boundary id {boundary_id}"))
}

fn state(element: &Element) -> String {
    element
        .get_attribute("data-krab-boundary-state")
        .unwrap_or_else(|| "<absent>".to_string())
}

/// SSR markup for the panicking island.
fn panic_island_ssr(boundary_id: &str) -> String {
    format!(
        concat!(
            r#"<div data-island="PanicIsland" data-props='{{}}' "#,
            r#"data-krab-boundary="PanicIsland" data-krab-boundary-id="{0}" "#,
            r#"data-krab-boundary-state="ssr"><span>server</span></div>"#
        ),
        boundary_id
    )
}

/// SSR markup for the healthy test-local `Counter` island (`support/islands.rs`).
///
/// No whitespace between elements — see the note in `hydration_browser.rs`.
fn counter_ssr(boundary_id: &str) -> String {
    format!(
        concat!(
            r#"<div data-island="Counter" data-props='{{"initial":0}}' "#,
            r#"data-krab-boundary="Counter" data-krab-boundary-id="{0}" "#,
            r#"data-krab-boundary-state="ssr">"#,
            r#"<button>Count: <span>0</span></button>"#,
            r#"</div>"#
        ),
        boundary_id
    )
}

/// Assert the contract for a boundary whose island panicked.
fn assert_failed_with_fallback(boundary: &Element) {
    let html = boundary.inner_html();
    assert_eq!(
        state(boundary),
        "error",
        "a panicking island must end in boundary state 'error'; DOM was: {html}"
    );
    assert!(
        html.contains("Hydration fallback rendered."),
        "a panicking island must render the documented fallback; DOM was: {html}"
    );
    assert!(
        html.contains("role=\"alert\""),
        "the fallback must be announced to assistive technology; DOM was: {html}"
    );
    assert!(
        !html.contains("server"),
        "the server-rendered content must be replaced, not left inert; DOM was: {html}"
    );
}

/// Runs first and asserts nothing about panics.
///
/// Its only job is attribution: if the panic tests below fail, this having
/// passed distinguishes "isolation regressed" from "the harness or the fixture
/// is broken".
#[wasm_bindgen_test]
fn canary_hydration_works_in_this_binary() {
    mount(&counter_ssr("canary"));
    krab_client::hydrate();

    assert_eq!(
        state(&island("canary")),
        "ok",
        "baseline hydration must work here before any panic test means anything"
    );
}

/// A panicking factory is contained: no trap escapes, the fallback renders.
#[wasm_bindgen_test]
fn a_panicking_island_factory_renders_the_fallback_instead_of_trapping() {
    mount(&panic_island_ssr("solo"));

    let trap = call_catching_traps(krab_client::hydrate);
    console::log_1(&format!("factory panic outcome: {trap:?}").into());

    assert_eq!(
        trap, None,
        "hydrate() must contain an island panic, not let the trap reach its caller"
    );
    assert_failed_with_fallback(&island("solo"));
}

/// The island isolation claim.
///
/// Three islands in DOM order: healthy, panicking, healthy. The first proves
/// the loop was running; the third proves the panic is contained to its own
/// boundary. Before 0.6.0 the third stayed at `ssr`.
#[wasm_bindgen_test]
fn a_healthy_island_after_a_panicking_one_still_hydrates() {
    let markup = format!(
        "{}{}{}",
        counter_ssr("before"),
        panic_island_ssr("boom"),
        counter_ssr("after")
    );
    mount(&markup);

    let trap = call_catching_traps(krab_client::hydrate);

    let before = state(&island("before"));
    let after = state(&island("after"));
    console::log_1(
        &format!("sibling survival: before={before} after={after} trap={trap:?}").into(),
    );

    assert_eq!(trap, None, "no trap may escape hydrate()");
    assert_eq!(
        before, "ok",
        "the island ahead of the panicking one must have hydrated normally"
    );
    assert_failed_with_fallback(&island("boom"));
    assert_eq!(
        after, "ok",
        "an island after a panicking one must still hydrate"
    );

    // Hydrated, not merely stamped: a listener was bound for the later island.
    // The count includes the earlier island's closures too, so this only
    // proves the later one added at least one.
    assert!(
        krab_client::event_closure_count() >= 2,
        "both healthy counters must have bound their click listeners"
    );
}

/// Several panicking islands in one pass: each fails on its own, and the
/// healthy islands between them all hydrate. This is what exercises recovery
/// after a *previous* trap in the same pass.
#[wasm_bindgen_test]
fn every_panicking_island_in_a_pass_is_contained_separately() {
    let markup = format!(
        "{}{}{}{}{}",
        panic_island_ssr("p1"),
        counter_ssr("c1"),
        panic_island_ssr("p2"),
        counter_ssr("c2"),
        panic_island_ssr("p3"),
    );
    mount(&markup);

    let trap = call_catching_traps(krab_client::hydrate);
    assert_eq!(trap, None, "no trap may escape hydrate()");

    for id in ["p1", "p2", "p3"] {
        assert_failed_with_fallback(&island(id));
    }
    for id in ["c1", "c2"] {
        assert_eq!(
            state(&island(id)),
            "ok",
            "{id} must hydrate between failures"
        );
    }
}

/// `hydrate_island` is isolated too, and reports the terminal state.
#[wasm_bindgen_test]
fn hydrate_island_contains_a_panic_and_returns_error() {
    mount(&format!(
        "{}{}",
        panic_island_ssr("single-bad"),
        counter_ssr("single-good")
    ));

    let bad = island("single-bad");
    let returned = std::rc::Rc::new(std::cell::RefCell::new(None));
    let trap = {
        let bad = bad.clone();
        let returned = returned.clone();
        call_catching_traps(move || {
            *returned.borrow_mut() = krab_client::hydrate_island(&bad);
        })
    };
    assert_eq!(trap, None, "hydrate_island must not throw for a panic");
    assert_eq!(
        returned.borrow().as_deref(),
        Some("error"),
        "hydrate_island must return the boundary state it ended in"
    );
    assert_failed_with_fallback(&bad);

    // Only the element passed in is hydrated.
    assert_eq!(state(&island("single-good")), "ssr");
    assert_eq!(
        krab_client::hydrate_island(&island("single-good")).as_deref(),
        Some("ok")
    );

    // Not an island: nothing happens.
    assert_eq!(krab_client::hydrate_island(&test_root()), None);
}

/// `hydrate_within_selector` hydrates only its matches and counts failures.
#[wasm_bindgen_test]
fn hydrate_within_selector_hydrates_matches_and_counts_failures() {
    mount(&format!(
        "{}{}{}",
        counter_ssr("sel-critical"),
        panic_island_ssr("sel-bad"),
        counter_ssr("sel-deferred")
    ));

    let failed = krab_client::hydrate_within_selector(
        "#krab-panic-boundary-root [data-krab-boundary-id='sel-critical']",
    );
    assert_eq!(failed, 0, "a healthy match reports no failures");
    assert_eq!(state(&island("sel-critical")), "ok");
    assert_eq!(
        state(&island("sel-bad")),
        "ssr",
        "non-matches are untouched"
    );
    assert_eq!(state(&island("sel-deferred")), "ssr");

    // The later, broader pass skips what is already hydrated and reports the
    // one boundary that failed.
    let failed = krab_client::hydrate_within_selector("#krab-panic-boundary-root");
    assert_eq!(failed, 1, "exactly the panicking island failed");
    assert_eq!(state(&island("sel-critical")), "ok");
    assert_failed_with_fallback(&island("sel-bad"));
    assert_eq!(state(&island("sel-deferred")), "ok");

    // An invalid selector hydrates nothing and does not throw.
    assert_eq!(krab_client::hydrate_within_selector("[[not a selector"), 0);
}

/// The module keeps working after a contained panic: a later pass (a route
/// change, a re-entrant bootstrap) still hydrates.
#[wasm_bindgen_test]
fn the_module_still_functions_after_a_factory_panic() {
    mount(&panic_island_ssr("dead"));
    let trap = call_catching_traps(krab_client::hydrate);
    assert_eq!(trap, None);

    mount(&counter_ssr("recovered"));
    krab_client::hydrate();

    assert_eq!(
        state(&island("recovered")),
        "ok",
        "a later hydration pass must still work after a contained panic"
    );
}

// ── Reactive state after a trap ─────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct LeakedContext(u32);

thread_local! {
    /// What `ContextReader` saw when it hydrated.
    static READER_SAW: std::cell::RefCell<Option<Option<LeakedContext>>> =
        const { std::cell::RefCell::new(None) };
}

/// Provides a context inside its own scope, opens a batch, then panics — so
/// the trap abandons both the owner guard and the batch guard.
fn context_then_panic_factory(_props: String) -> krab_core::Node {
    krab_core::signal::with_owner(|| {
        krab_core::signal::provide_context(LeakedContext(7));
        krab_core::signal::batch(|| -> krab_core::Node {
            panic!("{}", PANIC_MESSAGE);
        })
    })
}

/// Records whether it can see a `LeakedContext` from inside its own scope.
fn context_reader_factory(_props: String) -> krab_core::Node {
    krab_core::signal::with_owner(|| {
        let seen = krab_core::signal::use_context::<LeakedContext>();
        READER_SAW.with(|slot| *slot.borrow_mut() = Some(seen));
        krab_core::Node::Element(krab_core::Element {
            tag: "span".to_string(),
            attributes: vec![],
            children: vec![krab_core::Node::Text("reader".to_string())],
            events: vec![],
        })
    })
}

inventory::submit! {
    krab_client::IslandDefinition { name: "ContextThenPanic", factory: context_then_panic_factory }
}

inventory::submit! {
    krab_client::IslandDefinition { name: "ContextReader", factory: context_reader_factory }
}

/// A panicking island's scope must not become the parent of the next island's
/// scope, and its abandoned `batch` must not defer every later write.
#[wasm_bindgen_test]
async fn a_trapped_island_leaks_neither_its_context_nor_its_batch() {
    mount(concat!(
        r#"<div data-island="ContextThenPanic" data-props='{}' "#,
        r#"data-krab-boundary="ContextThenPanic" data-krab-boundary-id="leaky" "#,
        r#"data-krab-boundary-state="ssr"><span>server</span></div>"#,
        r#"<div data-island="ContextReader" data-props='{}' "#,
        r#"data-krab-boundary="ContextReader" data-krab-boundary-id="reader" "#,
        r#"data-krab-boundary-state="ssr"><span>reader</span></div>"#
    ));
    READER_SAW.with(|slot| *slot.borrow_mut() = None);

    let trap = call_catching_traps(krab_client::hydrate);
    assert_eq!(trap, None);
    assert_failed_with_fallback(&island("leaky"));
    assert_eq!(state(&island("reader")), "ok");
    assert_eq!(
        READER_SAW.with(|slot| slot.borrow().clone()),
        Some(None),
        "the next island must not see a context provided by the island that trapped"
    );
    assert!(
        krab_core::signal::Owner::current().is_none(),
        "no scope may be left current after hydration"
    );

    // Reactivity still delivers: a batch depth stuck at 1 would hold this
    // effect's re-run forever.
    let (value, set_value) = krab_core::signal::create_signal(0);
    let seen = std::rc::Rc::new(std::cell::Cell::new(-1));
    let sink = seen.clone();
    krab_core::signal::create_effect(move || sink.set(value.get()));
    set_value.set(9);
    support::settle().await;
    assert_eq!(seen.get(), 9, "signal writes must still reach effects");
}
