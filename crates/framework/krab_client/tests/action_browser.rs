//! Browser tests for [`krab_core::action::Action`], re-exported as
//! [`krab_client::Action`].
//!
//! `Action` lives in `krab_core` so an island can use it without a wasm32-only
//! dependency, but its dispatch path is only *live* on wasm32 — off the browser
//! `spawn_local_task` has no executor to hand the future to. That is why the
//! state machine is exercised here, in `krab_client`'s browser suite, rather
//! than as a native unit test in `krab_core`.
//!
//! The operations are plain async closures rather than real `#[server]` calls,
//! so what is under test is the state machine, not the network.

#![cfg(target_arch = "wasm32")]

use krab_client::create_action;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// Yield to the microtask queue so a spawned future can make progress.
async fn tick() {
    let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
    let _ = JsFuture::from(promise).await;
}

/// Several ticks, for futures that await more than once internally.
async fn settle() {
    for _ in 0..8 {
        tick().await;
    }
}

#[wasm_bindgen_test]
async fn a_successful_dispatch_moves_through_pending_to_value() {
    let action = create_action(|input: u32| async move { Ok::<_, String>(input * 2) });

    assert!(!action.pending().get(), "idle before dispatch");
    assert_eq!(action.value().get(), None);

    action.dispatch(21);
    assert!(
        action.pending().get(),
        "pending must be observable synchronously, before any await"
    );

    settle().await;

    assert!(!action.pending().get(), "pending must clear on completion");
    assert_eq!(action.value().get(), Some(42));
    assert_eq!(action.error().get(), None);
}

#[wasm_bindgen_test]
async fn a_failed_dispatch_populates_error_and_clears_pending() {
    let action = create_action(|_: ()| async move { Err::<u32, _>("upstream refused") });

    action.dispatch(());
    settle().await;

    assert!(!action.pending().get());
    assert_eq!(action.error().get().as_deref(), Some("upstream refused"));
    assert_eq!(action.value().get(), None);
}

/// A failed retry should not blank out data already on screen.
#[wasm_bindgen_test]
async fn a_failure_after_a_success_keeps_the_previous_value() {
    let should_fail = Rc::new(RefCell::new(false));
    let flag = should_fail.clone();

    let action = create_action(move |_: ()| {
        let fail = *flag.borrow();
        async move {
            if fail {
                Err("second attempt failed")
            } else {
                Ok(7u32)
            }
        }
    });

    action.dispatch(());
    settle().await;
    assert_eq!(action.value().get(), Some(7));

    *should_fail.borrow_mut() = true;
    action.dispatch(());
    settle().await;

    assert_eq!(
        action.value().get(),
        Some(7),
        "a failed retry must not discard the last good value"
    );
    assert_eq!(
        action.error().get().as_deref(),
        Some("second attempt failed")
    );
}

/// A new dispatch clears the previous error, so a stale message does not sit
/// next to a fresh in-flight request.
#[wasm_bindgen_test]
async fn dispatching_again_clears_the_previous_error() {
    let should_fail = Rc::new(RefCell::new(true));
    let flag = should_fail.clone();

    let action = create_action(move |_: ()| {
        let fail = *flag.borrow();
        async move {
            if fail {
                Err("first failed")
            } else {
                Ok(1u32)
            }
        }
    });

    action.dispatch(());
    settle().await;
    assert!(action.error().get().is_some());

    *should_fail.borrow_mut() = false;
    action.dispatch(());
    assert_eq!(
        action.error().get(),
        None,
        "the error must clear when a new dispatch starts, not when it finishes"
    );

    settle().await;
    assert_eq!(action.value().get(), Some(1));
    assert_eq!(action.error().get(), None);
}

/// The case the generation counter exists for.
///
/// Two dispatches where the *first* finishes last. Without a generation check
/// the stale result overwrites the newer one and `pending` reads false, so the
/// UI looks settled while showing the wrong answer.
#[wasm_bindgen_test]
async fn a_slow_earlier_dispatch_cannot_overwrite_a_later_one() {
    // Each call awaits a different number of microtasks: the first is slower.
    let call = Rc::new(RefCell::new(0u32));
    let counter = call.clone();

    let action = create_action(move |_: ()| {
        let n = *counter.borrow();
        *counter.borrow_mut() += 1;
        async move {
            let delay = if n == 0 { 6 } else { 0 };
            for _ in 0..delay {
                let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
                let _ = JsFuture::from(promise).await;
            }
            Ok::<_, String>(n)
        }
    });

    action.dispatch(()); // generation 1, slow
    action.dispatch(()); // generation 2, fast
    settle().await;

    assert_eq!(
        action.value().get(),
        Some(1),
        "the latest dispatch must win; the slow earlier one must be discarded"
    );
    assert!(
        !action.pending().get(),
        "pending must reflect the latest dispatch"
    );
}

#[wasm_bindgen_test]
async fn clones_share_one_operation_and_one_set_of_signals() {
    let action = create_action(|input: u32| async move { Ok::<_, String>(input + 1) });
    let observer = action.clone();

    action.dispatch(41);
    assert!(
        observer.pending().get(),
        "a clone must observe the same pending state"
    );

    settle().await;
    assert_eq!(observer.value().get(), Some(42));
}
