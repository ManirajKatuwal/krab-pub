//! Browser integration tests for signal cycle detection, microtask flush limits,
//! and scoped effect disposal on `wasm32`.

#![cfg(target_arch = "wasm32")]

use krab_client::{create_effect, create_effect_scoped, create_signal};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[path = "support/mod.rs"]
mod support;
use support::settle;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn wasm_self_write_converges() {
    let (count, set_count) = create_signal(0);
    let runs = Rc::new(Cell::new(0));
    let runs_clone = runs.clone();

    // `ReadSignal` is `Clone` but not `Copy`, so the closure gets its own
    // handle and the assertions below keep reading through `count`.
    let tracked = count.clone();
    create_effect(move || {
        let current = tracked.get();
        runs_clone.set(runs_clone.get() + 1);
        if current < 5 {
            set_count.set(current + 1);
        }
    });

    settle().await;

    assert_eq!(count.get(), 5, "converging self-write must reach target");
    assert_eq!(
        runs.get(),
        6,
        "effect must run once per increment + initial"
    );
}

#[wasm_bindgen_test]
async fn wasm_infinite_loop_cuts_off_at_flush_depth() {
    let (count, set_count) = create_signal(0);
    let runs = Rc::new(Cell::new(0));
    let runs_clone = runs.clone();

    // Unconditional self-write: without the microtask chain cap, this would hang the browser.
    create_effect(move || {
        let current = count.get();
        runs_clone.set(runs_clone.get() + 1);
        set_count.set(current + 1);
    });

    settle().await;

    assert!(
        runs.get() > 0 && runs.get() <= 65,
        "unconditional loop must be bounded by MAX_FLUSH_DEPTH (got {})",
        runs.get()
    );
}

#[wasm_bindgen_test]
async fn wasm_mutual_recursion_cuts_off() {
    let (sig_a, set_sig_a) = create_signal(0);
    let (sig_b, set_sig_b) = create_signal(0);
    let runs_a = Rc::new(Cell::new(0));
    let runs_b = Rc::new(Cell::new(0));

    let r_a = runs_a.clone();
    create_effect(move || {
        let b = sig_b.get();
        r_a.set(r_a.get() + 1);
        set_sig_a.set(b + 1);
    });

    let r_b = runs_b.clone();
    create_effect(move || {
        let a = sig_a.get();
        r_b.set(r_b.get() + 1);
        set_sig_b.set(a + 1);
    });

    settle().await;

    assert!(
        runs_a.get() + runs_b.get() <= 130,
        "mutual recursion chain must be capped (runs_a={}, runs_b={})",
        runs_a.get(),
        runs_b.get()
    );
}

/// The other side of the cutoff: breadth is not depth.
///
/// `WIDTH` effects each write a *different* signal in response to one source
/// change. That is a two-generation flush, not a chain — but the cap used to
/// count effect-originated writes cumulatively within a flush and never
/// decrement, so write 64 tripped `MAX_FLUSH_DEPTH` and every delivery after
/// it was dropped: a stale DOM with nothing but an error log to show for it.
#[wasm_bindgen_test]
async fn wasm_wide_effect_fan_out_delivers_every_effect() {
    const WIDTH: usize = 96;

    let (source, set_source) = create_signal(0);
    let delivered = Rc::new(Cell::new(0usize));

    for _ in 0..WIDTH {
        let (mirror, set_mirror) = create_signal(0);

        // One effect-originated write per source change.
        let tracked = source.clone();
        create_effect(move || {
            set_mirror.set(tracked.get());
        });

        // Counts the deliveries that actually landed.
        let seen = delivered.clone();
        create_effect(move || {
            let _ = mirror.get();
            seen.set(seen.get() + 1);
        });
    }

    settle().await;
    assert_eq!(
        delivered.get(),
        WIDTH,
        "each reader must run once when it is created"
    );

    delivered.set(0);
    set_source.set(1);
    settle().await;

    assert_eq!(
        delivered.get(),
        WIDTH,
        "all {WIDTH} effect-originated writes must be delivered, not just the first MAX_FLUSH_DEPTH"
    );
}

#[wasm_bindgen_test]
async fn wasm_scoped_effect_disposal_cancels_subscription() {
    let (count, set_count) = create_signal(10);
    let runs = Rc::new(Cell::new(0));
    let runs_clone = runs.clone();

    let handle = create_effect_scoped(move || {
        let _ = count.get();
        runs_clone.set(runs_clone.get() + 1);
    });

    settle().await;
    assert_eq!(runs.get(), 1, "initial execution");

    set_count.set(20);
    settle().await;
    assert_eq!(runs.get(), 2, "execution after update");

    handle.dispose();

    set_count.set(30);
    settle().await;
    assert_eq!(
        runs.get(),
        2,
        "disposed effect must not execute on subsequent signal changes"
    );
}
