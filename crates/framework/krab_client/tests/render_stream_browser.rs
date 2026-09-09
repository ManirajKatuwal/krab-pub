//! Pins that the suspense-marker parser in `krab_core::render_stream` is
//! reachable from `wasm32`.
//!
//! This is a contract test, not a behaviour test — the parser has its own unit
//! tests on the host. What only a `wasm32` build can prove is that the module
//! still *exports* the parser there. The 0.5.0 release first gated all of
//! `render_stream` off `wasm32` to keep `ChunkedStreamWriter`'s `Instant` out of
//! the browser bundle, and in doing so removed `SuspenseMarker::parse` and
//! `is_finalized_ssr_snapshot` — pure string parsing that had compiled and run
//! on `wasm32` since they existed — while the release notes said nothing that
//! worked could break. The gate now sits on the writer items inside the module.
//!
//! A future "tidy-up" that moves it back to the `pub mod` fails this suite
//! rather than shipping the same break twice. Nothing in `krab_client` itself
//! calls the parser, so without this file the linker would drop it and no gate
//! would notice.
//!
//! Run with the same command as `hydration_browser`; see that file's docs.

#![cfg(target_arch = "wasm32")]

use krab_core::render_stream::{is_finalized_ssr_snapshot, SuspenseState};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

// `SuspenseMarker` is deprecated for downstream callers in favour of
// `is_finalized_ssr_snapshot`; both are exercised here because both are the
// public surface this test guards, and the deprecation removes the type in 0.6.0
// — at which point the import and the assertion on it go with it. The import is
// inside the function so the `allow` visibly covers it: this crate's wasm32
// clippy runs with `-D warnings`, which turns a deprecated name in a
// module-level `use` into a hard error the function-level allow cannot reach.
#[allow(deprecated)]
#[wasm_bindgen_test]
fn the_marker_parser_is_reachable_from_wasm32() {
    use krab_core::render_stream::SuspenseMarker;

    let marker = SuspenseMarker::parse("<!--krab:suspense:b1:resolved-->")
        .expect("a well-formed marker parses on wasm32");
    assert_eq!(marker.boundary_id, "b1");
    assert_eq!(marker.state, SuspenseState::Resolved);

    assert!(is_finalized_ssr_snapshot(
        "<p>x</p><!--krab:suspense:b1:pending--><!--krab:suspense:b1:resolved-->"
    ));
    assert!(!is_finalized_ssr_snapshot(
        "<!--krab:suspense:b1:pending-->"
    ));
}
