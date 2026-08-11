//! Browser tests for [`krab_core::resource::Resource`], re-exported as
//! [`krab_client::Resource`].
//!
//! Like `Action`, the dispatch path is only live on wasm32 — natively there is
//! no executor, and `krab_core`'s own `resource::tests` pin that inertness.
//! What runs here is the live half of ADR 0009: fetch-on-creation,
//! initial-suppresses-the-first-fetch, source-tracked refetching, and the
//! generation counter.
//!
//! Fetchers are plain async closures, not real `#[server]` calls: under test is
//! the state machine, not the network.

#![cfg(target_arch = "wasm32")]

use krab_client::{create_resource, create_resource_with_initial, ResourceState};
use krab_core::signal::create_signal;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[path = "support/mod.rs"]
mod support;
use support::settle;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn a_resource_without_initial_fetches_on_creation() {
    let (id, _set_id) = create_signal(21u32);
    let resource = create_resource(
        move || id.get(),
        |n: u32| async move { Ok::<_, String>(n * 2) },
    );

    assert!(
        resource.state().get().is_pending(),
        "pending until the fetch resolves"
    );

    settle().await;

    assert!(resource.state().get().is_ready());
    assert_eq!(resource.value().get(), Some(42));
}

/// The ADR 0009 hydration contract: a server-provided value means no request
/// on mount — otherwise every load the server just did is doubled.
#[wasm_bindgen_test]
async fn an_initial_value_starts_ready_and_suppresses_the_first_fetch() {
    let fetches = Rc::new(RefCell::new(0u32));
    let counter = fetches.clone();

    let (id, _set_id) = create_signal(1u32);
    let resource = create_resource_with_initial(
        Some(7u32),
        move || id.get(),
        move |n: u32| {
            *counter.borrow_mut() += 1;
            async move { Ok::<_, String>(n) }
        },
    );

    assert!(resource.state().get().is_ready());
    assert_eq!(resource.value().get(), Some(7));

    settle().await;

    assert_eq!(
        *fetches.borrow(),
        0,
        "hydrating with an initial value must not refetch on mount"
    );
    assert_eq!(resource.value().get(), Some(7));
}

/// The source closure is tracked: a change to a signal it reads refetches,
/// even when the first fetch was suppressed by an initial value.
#[wasm_bindgen_test]
async fn a_changed_source_triggers_a_refetch() {
    let (id, set_id) = create_signal(1u32);
    let resource = create_resource_with_initial(
        Some(100u32),
        move || id.get(),
        |n: u32| async move { Ok::<_, String>(n * 10) },
    );

    assert_eq!(resource.value().get(), Some(100));

    set_id.set(5);
    settle().await;

    assert!(resource.state().get().is_ready());
    assert_eq!(
        resource.value().get(),
        Some(50),
        "a source change must run the fetcher with the new source value"
    );
}

/// During a refetch, `state` reports the reload while `value` keeps the data
/// on screen — a spinner beside the stale list, not a blank page.
#[wasm_bindgen_test]
async fn a_refetch_reports_pending_without_blanking_the_value() {
    let (id, _set_id) = create_signal(3u32);
    let resource = create_resource_with_initial(
        Some(3u32),
        move || id.get(),
        |n: u32| async move { Ok::<_, String>(n) },
    );

    resource.refetch();

    assert!(
        resource.state().get().is_pending(),
        "state must reflect the in-flight refetch"
    );
    assert_eq!(
        resource.value().get(),
        Some(3),
        "the previous value must stay while the refetch runs"
    );

    settle().await;
    assert!(resource.state().get().is_ready());
}

/// A failed refetch must not discard data already on screen.
#[wasm_bindgen_test]
async fn a_failed_refetch_moves_state_to_error_but_keeps_the_value() {
    let should_fail = Rc::new(RefCell::new(false));
    let flag = should_fail.clone();

    let (id, _set_id) = create_signal(1u32);
    let resource = create_resource(
        move || id.get(),
        move |n: u32| {
            let fail = *flag.borrow();
            async move {
                if fail {
                    Err("upstream refused".to_string())
                } else {
                    Ok(n)
                }
            }
        },
    );

    settle().await;
    assert_eq!(resource.value().get(), Some(1));

    *should_fail.borrow_mut() = true;
    resource.refetch();
    settle().await;

    assert_eq!(
        resource.state().get(),
        ResourceState::Error("upstream refused".to_string())
    );
    assert_eq!(
        resource.value().get(),
        Some(1),
        "a failed refetch must not discard the last good value"
    );
}

/// The case the generation counter exists for: two fetches where the first
/// finishes last. Without the check, the stale response overwrites the newer
/// one and the resource reads `Ready` while showing the wrong data.
#[wasm_bindgen_test]
async fn a_slow_earlier_fetch_cannot_overwrite_a_later_one() {
    let call = Rc::new(RefCell::new(0u32));
    let counter = call.clone();

    let (id, _set_id) = create_signal(0u32);
    let resource = create_resource(
        move || id.get(),
        move |_: u32| {
            let n = *counter.borrow();
            *counter.borrow_mut() += 1;
            async move {
                // The first call awaits more microtasks: it is slower.
                let delay = if n == 0 { 6 } else { 0 };
                for _ in 0..delay {
                    let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
                    let _ = JsFuture::from(promise).await;
                }
                Ok::<_, String>(n)
            }
        },
    );

    // Creation fetch (call 0, slow) is in flight; refetch (call 1, fast).
    resource.refetch();
    settle().await;

    assert_eq!(
        resource.value().get(),
        Some(1),
        "the latest fetch must win; the slow earlier one must be discarded"
    );
    assert!(resource.state().get().is_ready());
}

#[wasm_bindgen_test]
async fn clones_share_one_fetch_and_one_set_of_signals() {
    let (id, _set_id) = create_signal(20u32);
    let resource = create_resource(
        move || id.get(),
        |n: u32| async move { Ok::<_, String>(n + 1) },
    );
    let observer = resource.clone();

    assert!(observer.state().get().is_pending());

    settle().await;
    assert_eq!(observer.value().get(), Some(21));
}
