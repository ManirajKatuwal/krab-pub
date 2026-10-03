//! Per-island panic isolation on a `panic = "abort"` target.
//!
//! `wasm32-unknown-unknown` has no unwinding runtime: a panic runs the panic
//! hook and then executes `unreachable`, which traps. A trap cannot be observed
//! from Rust — `catch_unwind` never returns `Err` — but it *can* be observed from
//! JavaScript, where it surfaces as a `RuntimeError` thrown out of whichever
//! wasm call was on the stack. So each island is hydrated through a small JS
//! trampoline, `krab_call_isolated`, which calls back into wasm inside a
//! `try`/`catch`:
//!
//! ```text
//! hydrate_within (wasm) ── krab_call_isolated (JS, try) ── island body (wasm)
//!        ▲                          │ catch                      │ panic → trap
//!        └──────── Err(message) ────┘ ◀──────────────────────────┘
//! ```
//!
//! The trap discards only the wasm frames *above* the trampoline. The loop over
//! islands sits below it, so it sees an `Err`, stamps that one boundary
//! `error`, and carries on with the next island. Before this, the trap escaped
//! the loop itself and every later island stayed at `ssr`, silently.
//!
//! The trampoline is shipped as a wasm-bindgen `inline_js` snippet rather than
//! built with `new Function`, so it works under a Content-Security-Policy that
//! forbids `unsafe-eval`, and no consumer has to write it.
//!
//! # Where the message comes from
//!
//! A trap carries no payload — the JS side only sees `RuntimeError: unreachable`.
//! The panic hook, however, runs *before* the trap, so this module installs one
//! (chained in front of whatever hook was already set) that records the panic
//! message while an isolated call is in flight. If the application replaces the
//! hook afterwards, the record is simply missing and the JS error text is used.
//!
//! # Recovery is best-effort
//!
//! The wasm instance stays callable after a trap — nothing is re-instantiated —
//! but it is not necessarily *consistent*. Everything the trapped frames were
//! doing is abandoned mid-flight:
//!
//! - their heap allocations leak, and their shadow-stack space is not returned;
//! - a `RefCell` borrowed at the moment of the trap stays borrowed, so a later
//!   use of the same cell panics (and is itself contained, one island at a
//!   time);
//! - no drop guard runs, so `krab_core`'s reactive position — current owner,
//!   current subscriber, `batch` and flush depths — is left as the dead island
//!   had it. The hydration loop therefore snapshots it before each isolated
//!   call (`krab_core::signal::snapshot_reactive_state`) and restores it after
//!   a trap; without that the next island's scope was parented to the dead
//!   one, and a stuck batch depth deferred every later signal write.
//!
//! This is acceptable because it only ever happens on a failure path that was
//! strictly worse before — one bad island used to take every later island with
//! it. A bundle built with `panic = "unwind"` (nightly `-Z build-std` with the
//! exception-handling proposal) would not need any of this; wasm-bindgen's
//! `--force-enable-abort-handler` flag, which poisons the instance on the first
//! trap, defeats it and must not be used with this runtime.

use std::cell::{Cell, RefCell};
use std::sync::Once;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::Element;

// `undefined` means the body returned normally. Anything else is what was
// thrown — a `RuntimeError` for a Rust panic, or a JS exception that escaped a
// `web-sys` call not declared `catch`. A literal `throw undefined` is mapped to
// `null` so it is still reported as a failure.
#[wasm_bindgen(inline_js = "export function krab_call_isolated(body, element) {\n\
    try {\n\
        body(element);\n\
        return undefined;\n\
    } catch (error) {\n\
        return error === undefined ? null : error;\n\
    }\n\
}\n")]
extern "C" {
    #[wasm_bindgen(js_name = krab_call_isolated)]
    fn krab_call_isolated(body: &Closure<dyn FnMut(Element)>, element: &Element) -> JsValue;
}

thread_local! {
    /// How many isolated calls are on the stack. The hook records only while
    /// this is non-zero, so a panic somewhere unrelated — an event handler
    /// long after hydration — does not leave a stale record behind.
    ///
    /// A depth rather than a flag because the outer frame decrements it after
    /// the trampoline returns, which still happens after a trap; the frames
    /// that trapped never get to.
    static ISOLATION_DEPTH: Cell<u32> = const { Cell::new(0) };

    /// The message of the last panic seen inside an isolated call.
    static PANIC_RECORD: RefCell<Option<String>> = const { RefCell::new(None) };
}

static HOOK: Once = Once::new();

/// Chain a recording hook in front of the current one, once per instance.
///
/// Installed lazily on the first isolated call rather than at start-up, so an
/// application that sets its own hook (`console_error_panic_hook`, typically)
/// before hydrating keeps it: the previous hook is taken and still called.
fn install_panic_hook() {
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            record_panic(info);
            previous(info);
        }));
    });
}

/// Record `info` for the enclosing isolated call, if there is one.
///
/// Every access is fallible on purpose. A panic *inside* a panic hook aborts
/// without running any hook at all, so this must not be able to panic even if
/// a trap has left the record cell borrowed.
fn record_panic(info: &std::panic::PanicHookInfo<'_>) {
    let inside = ISOLATION_DEPTH
        .try_with(|depth| depth.get() > 0)
        .unwrap_or(false);
    if !inside {
        return;
    }

    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    let message = match info.location() {
        Some(location) => format!("{message} at {location}"),
        None => message,
    };

    let _ = PANIC_RECORD.try_with(|record| {
        if let Ok(mut record) = record.try_borrow_mut() {
            *record = Some(message);
        }
    });
}

/// Take the recorded message, tolerating a cell a trap left borrowed.
fn take_panic_record() -> Option<String> {
    PANIC_RECORD
        .try_with(|record| record.try_borrow_mut().ok().and_then(|mut r| r.take()))
        .ok()
        .flatten()
}

/// Run `body(element)` so that a panic or JS exception inside it is contained.
///
/// Returns `Err` with a description when the call trapped or threw. The frames
/// inside `body` are gone by then; the caller is responsible for putting the
/// boundary into a terminal state.
pub(crate) fn call_isolated(
    body: &Closure<dyn FnMut(Element)>,
    element: &Element,
) -> Result<(), String> {
    install_panic_hook();
    // A record left over from a panic outside any isolated call — or from an
    // earlier call whose hook ran but whose trap was caught elsewhere — must
    // not be attributed to this island.
    let _ = take_panic_record();

    ISOLATION_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let outcome = krab_call_isolated(body, element);
    ISOLATION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));

    if outcome.is_undefined() {
        return Ok(());
    }

    Err(take_panic_record().unwrap_or_else(|| describe_js_error(&outcome)))
}

/// A readable description of whatever JS threw.
fn describe_js_error(error: &JsValue) -> String {
    if let Some(text) = error.as_string() {
        return text;
    }
    // A trap is a `WebAssembly.RuntimeError`, which is an `Error`, so this
    // yields "RuntimeError: unreachable" for a panic whose message the hook
    // could not record, and "Name: message" for a thrown JS error.
    match error.dyn_ref::<js_sys::Error>() {
        Some(error) => String::from(error.to_string()),
        None => format!("{error:?}"),
    }
}
