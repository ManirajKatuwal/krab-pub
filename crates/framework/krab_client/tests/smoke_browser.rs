//! Canary for the browser test harness itself.
//!
//! Kept deliberately. When a browser test fails, the first question is whether
//! the *runtime* broke or the *harness* did, and those failures look nothing
//! alike once you know the difference but identical when you do not. This file
//! answers that question in two seconds.
//!
//! - These pass, `hydration_browser` fails → the runtime or those tests.
//! - These fail too → the harness, the driver, or the wasm-bindgen versions.
//!   Check that the `wasm-bindgen-cli` on PATH matches `Cargo.lock`.
//!
//! The second test exists because a canary that touches no crate symbol proves
//! almost nothing: the linker drops the crate and the harness runs an
//! effectively empty module. Calling into `krab_client` forces its
//! wasm-bindgen glue and its `#[wasm_bindgen(start)]` hook to link and execute.
//!
//! Run with the same command as `hydration_browser`; see that file's docs.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn the_harness_can_run_a_test_at_all() {
    // Deliberately trivial, but not `assert_eq!(1 + 1, 2)` — clippy's `eq_op`
    // rejects that as a constant comparison, and this crate builds with
    // `-D warnings`.
    assert!(web_sys::window().is_some(), "no window in the test page");
}

#[wasm_bindgen_test]
fn krab_client_glue_links_and_hydrate_is_callable() {
    // No islands in the document, so this is a no-op — the point is that the
    // crate's module initialisation runs without throwing.
    krab_client::hydrate();
}

/// Guards the trap that cost the most time getting this suite running.
///
/// `wasm-bindgen-test` renders its own progress and results into
/// `document.body`. A test that replaces the body's HTML destroys the harness's
/// output element, and the run then reports
/// `Failed to detect test as having been run. It might have timed out.` —
/// which looks like a driver or toolchain fault, not a test bug.
///
/// `hydration_browser::mount` therefore mounts into its own container. If that
/// ever regresses to writing `body.innerHTML`, this test still passes but the
/// others start "timing out"; the comment here is the breadcrumb back.
#[wasm_bindgen_test]
fn the_harness_owns_document_body() {
    let body = web_sys::window()
        .expect("no window")
        .document()
        .expect("no document")
        .body()
        .expect("no body");

    assert!(
        body.child_element_count() > 0,
        "the harness renders its output into <body>; tests must not clear it"
    );
}
