//! Pins that the suspense-marker vocabulary in `krab_core::render_stream` is
//! reachable from `wasm32`.
//!
//! This is a contract test, not a behaviour test — the parser has its own unit
//! tests on the host. What only a `wasm32` build can prove is that the module
//! still *exports* it there. The 0.5.0 release first gated all of
//! `render_stream` off `wasm32` to keep `ChunkedStreamWriter`'s `Instant` out of
//! the browser bundle, and in doing so removed `is_finalized_ssr_snapshot` —
//! pure string parsing that had compiled and run on `wasm32` since it existed —
//! while the release notes said nothing that worked could break. The gate now
//! sits on the writer items inside the module.
//!
//! A future "tidy-up" that moves it back to the `pub mod` fails this suite
//! rather than shipping the same break twice. Nothing in `krab_client` itself
//! calls the parser, so without this file the linker would drop it and no gate
//! would notice. (`SuspenseMarker`, deprecated in 0.5.0, was removed in 0.6.0;
//! its parser is now private behind `is_finalized_ssr_snapshot`.)
//!
//! Run with the same command as `hydration_browser`; see that file's docs.

#![cfg(target_arch = "wasm32")]

use krab_core::render_stream::{is_finalized_ssr_snapshot, SuspenseState};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn the_marker_vocabulary_is_reachable_from_wasm32() {
    // `SuspenseState` is part of the reachable surface; naming a variant keeps
    // the import load-bearing.
    let _ = SuspenseState::Resolved;

    assert!(is_finalized_ssr_snapshot(
        "<p>x</p><!--krab:suspense:b1:pending--><!--krab:suspense:b1:resolved-->"
    ));
    assert!(!is_finalized_ssr_snapshot(
        "<!--krab:suspense:b1:pending-->"
    ));
}
