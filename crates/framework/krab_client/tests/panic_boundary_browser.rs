//! Browser tests for the island factory panic boundary in [`krab_client::hydrate`].
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
//! `hydrate()` wraps every island factory call in
//! `std::panic::catch_unwind(AssertUnwindSafe(..))`. Its `Err` arm logs
//! `factory_panic`, sets `data-krab-boundary-state="error"` and renders
//! `<div role="alert">Hydration fallback rendered.</div>`. Nothing exercised
//! that arm, so nothing established whether it can ever be reached.
//!
//! It cannot. `rustc --print cfg --target wasm32-unknown-unknown` reports
//! `panic="abort"`: the target has no unwinding runtime, so a panic inside a
//! factory runs the panic hook and then executes `unreachable`, trapping the
//! whole wasm instance. `catch_unwind` never observes an `Err`, the fallback
//! markup never renders, and — the part that matters for the island isolation
//! claim — the `for` loop over islands never reaches the next island.
//!
//! These are **characterization tests**: they assert the behaviour that exists,
//! not the behaviour the code appears to promise. Each carries a
//! `RECOVERED`/`ABORTED` branch so that the day the panic strategy changes
//! (`-Z build-std` with the exception-handling proposal, or a future stable
//! `panic=unwind` for wasm32) the branch flips and the assertions describe the
//! new reality instead of silently passing.
//!
//! # How the trap is observed without killing the runner
//!
//! A wasm trap surfaces at the JS boundary as a `RuntimeError`. Calling
//! `hydrate()` directly from a test body lets that error escape into the
//! harness, which reports it as an opaque test failure and tells you nothing
//! about the DOM. Instead the call is routed through a two-line JS trampoline
//! that wraps it in `try`/`catch`, so the test survives the trap and can go on
//! to inspect exactly which islands hydrated and which did not.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{console, Element};

#[path = "support/mod.rs"]
mod support;
use support::document;

wasm_bindgen_test_configure!(run_in_browser);

/// This suite's own container, kept distinct from the other suites'.
const TEST_ROOT_ID: &str = "krab-panic-boundary-root";

/// An island whose factory panics the moment hydration calls it.
///
/// This is the only thing in the crate's test surface that reaches the `Err`
/// arm of the `catch_unwind` in `hydrate()` — or would, if that arm were
/// reachable.
fn panicking_factory(_props: String) -> krab_core::Node {
    panic!("intentional island factory panic from panic_boundary_browser.rs");
}

inventory::submit! {
    krab_client::IslandDefinition { name: "PanicIsland", factory: panicking_factory }
}

/// Call `f` from JavaScript inside a `try`/`catch`.
///
/// Returns `None` if `f` returned normally, or `Some(description)` if it
/// trapped. Under `panic="abort"` a Rust panic becomes a wasm `unreachable`
/// trap, which crosses back into JS as a `RuntimeError`; catching it there is
/// the only way a test can both trigger the panic and then look at the DOM.
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

/// SSR markup for the healthy `Counter` island shipped in this crate.
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

/// Runs first and asserts nothing about panics.
///
/// Its only job is attribution: if the panic tests below take the runner down,
/// this having passed distinguishes "the wasm module aborted" from "the harness
/// or the fixture is broken".
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

/// The headline question: does `catch_unwind` recover a panicking factory?
#[wasm_bindgen_test]
fn a_panicking_island_factory_aborts_instead_of_rendering_the_fallback() {
    mount(&panic_island_ssr("solo"));

    let trap = call_catching_traps(krab_client::hydrate);
    console::log_1(&format!("factory panic outcome: {trap:?}").into());

    let boundary = island("solo");
    let observed_state = state(&boundary);
    let observed_html = boundary.inner_html();
    console::log_1(&format!("boundary state={observed_state} html={observed_html}").into());

    match trap {
        None => {
            // RECOVERED: unwinding reached `catch_unwind`. The documented
            // fallback contract applies and must hold exactly.
            assert_eq!(
                observed_state, "error",
                "a recovered factory panic must publish boundary state 'error'"
            );
            assert!(
                observed_html.contains("Hydration fallback rendered."),
                "a recovered factory panic must render the documented fallback; DOM was: {observed_html}"
            );
            assert!(
                observed_html.contains("role=\"alert\""),
                "the fallback must be announced to assistive technology; DOM was: {observed_html}"
            );
        }
        Some(description) => {
            // ABORTED: wasm32-unknown-unknown is `panic="abort"`, so the panic
            // became a trap that escaped `hydrate()` entirely. The `Err` arm of
            // the `catch_unwind` in `lib.rs` never ran.
            assert!(
                observed_state != "error",
                "boundary state 'error' would mean the Err arm ran, contradicting the trap: {description}"
            );
            assert!(
                !observed_html.contains("Hydration fallback rendered."),
                "the fallback cannot have rendered after an abort; DOM was: {observed_html}"
            );
            // The boundary is left in the transient state `hydrate()` stamps on
            // before calling the factory: visible, un-hydrated, mid-flight.
            assert_eq!(
                observed_state, "hydrating",
                "an aborted island is stranded in its pre-factory state"
            );
        }
    }
}

/// 4.2 — the island isolation claim.
///
/// Three islands in DOM order: healthy, panicking, healthy. `hydrate()` walks
/// them in that order, so the first proves the loop was running and the third
/// proves whether the panic is contained to its own boundary.
#[wasm_bindgen_test]
fn a_healthy_island_after_a_panicking_one_is_left_unhydrated() {
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

    assert_eq!(
        before, "ok",
        "the island ahead of the panicking one must have hydrated normally"
    );

    match trap {
        None => {
            // RECOVERED: containment holds, and the later island is unaffected.
            assert_eq!(
                after, "ok",
                "with a working boundary, a later island must still hydrate"
            );
        }
        Some(_) => {
            // ABORTED: the trap escaped the whole `for` loop over islands, so
            // every island after the panicking one is silently skipped. The
            // blast radius of one bad factory is the rest of the page.
            assert_eq!(
                after, "ssr",
                "a factory panic strands every later island untouched at its server-rendered state"
            );
        }
    }
}

/// The wasm instance survives the trap well enough to keep serving the page —
/// which is exactly why the failure is quiet rather than loud.
///
/// A trap unwinds only to the JS boundary; the instance and its memory stay
/// callable. So a later `hydrate()` (a route change, a re-entrant bootstrap)
/// still works, and the stranded islands from the aborted pass get picked up
/// only if they are still in the document. Nothing in the page reports that a
/// hydration pass died halfway.
#[wasm_bindgen_test]
fn the_module_still_functions_after_a_factory_panic() {
    mount(&panic_island_ssr("dead"));
    let _ = call_catching_traps(krab_client::hydrate);

    mount(&counter_ssr("recovered"));
    krab_client::hydrate();

    assert_eq!(
        state(&island("recovered")),
        "ok",
        "a later hydration pass must still work; the trap is scoped to its own call"
    );
}
