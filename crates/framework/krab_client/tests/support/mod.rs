//! Shared helpers for the `krab_client` browser suites.
//!
//! Integration tests are separate crates, so this cannot be a library module;
//! each suite includes it with `#[path = "support/mod.rs"] mod support;` and
//! compiles its own copy. Cargo does not treat `tests/support/` as a test
//! target of its own. Every suite uses a subset of what is here, hence the
//! `dead_code` allow.

#![allow(dead_code)]

use wasm_bindgen_futures::JsFuture;
use web_sys::{Document, Element};

/// Yield to the microtask queue so a spawned future can make progress.
pub async fn tick() {
    let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
    let _ = JsFuture::from(promise).await;
}

/// Several ticks, for futures that await more than once internally.
pub async fn settle() {
    for _ in 0..8 {
        tick().await;
    }
}

pub fn document() -> Document {
    web_sys::window()
        .expect("no window")
        .document()
        .expect("no document")
}

/// A freshly emptied container with the given id, created under `<body>` on
/// first use and reused after. Suites keep distinct ids on purpose — separate
/// containers per suite.
///
/// **Never write to `document.body.innerHTML` here.** `wasm-bindgen-test`
/// renders its own progress and results into the body; replacing the body's
/// HTML destroys the harness's output element, after which the runner reports
/// `Failed to detect test as having been run. It might have timed out.` — with
/// no indication that the tests themselves were fine. This cost a long
/// debugging detour through the driver and the toolchain before the cause
/// turned out to be the test helper.
pub fn mount_container(id: &str) -> Element {
    let doc = document();

    let root = match doc.query_selector(&format!("#{id}")) {
        Ok(Some(existing)) => existing,
        _ => {
            let created = doc
                .create_element("div")
                .expect("failed to create test container");
            created.set_id(id);
            doc.body()
                .expect("no body")
                .append_child(&created)
                .expect("failed to append test container");
            created
        }
    };

    root.set_inner_html("");
    root
}
