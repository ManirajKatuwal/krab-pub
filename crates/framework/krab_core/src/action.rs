//! Client-initiated async writes with observable state.
//!
//! A `#[server]` function is callable from an island, but calling one meant
//! hand-rolling everything around it: a signal for "is it running", another for
//! the result, another for the error, and remembering to clear them in the
//! right order. [`create_action`] packages that.
//!
//! ```ignore
//! let add = create_action(|title: String| async move { add_task(title).await });
//!
//! view! {
//!     <button on:click={ move |_| add.dispatch("write docs".to_string()) }>
//!         "Add"
//!     </button>
//!     // Attribute values are evaluated once at build and are not reactive, so
//!     // pending state renders through `<Show>`, not `disabled={ … }`.
//!     <Show when={move || add.pending().get()}>
//!         <span class="pending">"Saving…"</span>
//!     </Show>
//!     <Show when={move || add.error().get().is_some()}>
//!         <p class="error">{ move || add.error().get().unwrap_or_default() }</p>
//!     </Show>
//! }
//! ```
//!
//! # Native builds
//!
//! An `#[island]` body is compiled for **both** targets — natively to render the
//! markup, and for wasm to hydrate it — so an API available only on wasm forces
//! a `#[cfg(target_arch)]` back into application code, which is the wart
//! `krab_client::spawn` was added to remove. `Action` therefore exists on both.
//!
//! Natively, [`Action::dispatch`] does not run the operation: server-side
//! rendering produces markup, and nothing there can click a button. It returns
//! before touching a single signal, so `pending` reads false and `error` reads
//! `None` no matter how often it is called, and the SSR markup renders the idle
//! shape the browser will hydrate against.

use crate::signal::{batch, create_signal, ReadSignal, WriteSignal};
use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;

/// An async operation with `pending`, `value`, and `error` exposed as signals.
///
/// Cloning is cheap and shares one operation and one set of signals, so a
/// handler and the markup that reflects its state can hold their own copies.
pub struct Action<I, O: 'static> {
    pending: ReadSignal<bool>,
    value: ReadSignal<Option<O>>,
    error: ReadSignal<Option<String>>,
    set_pending: WriteSignal<bool>,
    set_value: WriteSignal<Option<O>>,
    set_error: WriteSignal<Option<String>>,
    run: Rc<dyn Fn(I) -> GuardedFuture<O>>,
    /// Bumped on every dispatch so a slow earlier call cannot overwrite a
    /// faster later one. Without it, dispatching twice and having the first
    /// request finish second leaves stale data on screen with `pending` false —
    /// which looks settled and is wrong.
    generation: Rc<Cell<u64>>,
}

use std::pin::Pin;

impl<I, O: 'static> Clone for Action<I, O> {
    fn clone(&self) -> Self {
        Self {
            pending: self.pending.clone(),
            value: self.value.clone(),
            error: self.error.clone(),
            set_pending: self.set_pending.clone(),
            set_value: self.set_value.clone(),
            set_error: self.set_error.clone(),
            run: self.run.clone(),
            generation: self.generation.clone(),
        }
    }
}

impl<I, O> Action<I, O>
where
    I: 'static,
    O: Clone + 'static,
{
    /// Whether a dispatch is in flight.
    pub fn pending(&self) -> ReadSignal<bool> {
        self.pending.clone()
    }

    /// The most recent successful result, if any.
    ///
    /// A failed dispatch leaves the previous value in place rather than
    /// clearing it: replacing rendered data with nothing because a retry failed
    /// is usually worse than showing the last good value beside the error.
    pub fn value(&self) -> ReadSignal<Option<O>> {
        self.value.clone()
    }

    /// The error from the most recent dispatch, cleared when a new one starts.
    pub fn error(&self) -> ReadSignal<Option<String>> {
        self.error.clone()
    }

    /// Start the operation. Returns immediately; watch [`pending`](Self::pending).
    ///
    /// Dispatching again while one is in flight does not cancel the first — the
    /// earlier future still runs to completion — but its result is discarded,
    /// so only the latest dispatch can write state.
    pub fn dispatch(&self, input: I) {
        let set_pending = self.set_pending.clone();
        let set_value = self.set_value.clone();
        let set_error = self.set_error.clone();

        run_guarded(
            &self.generation,
            // One update, so anything reading both `pending` and `error` sees a
            // single consistent transition rather than two.
            || {
                batch(|| {
                    self.set_pending.set(true);
                    self.set_error.set(None);
                })
            },
            || (self.run)(input),
            move |outcome| {
                set_pending.set(false);
                match outcome {
                    Ok(value) => set_value.set(Some(value)),
                    Err(message) => set_error.set(Some(message)),
                }
            },
        );
    }
}

/// Wrap an async operation as an [`Action`].
///
/// `f` is typically a `#[server]` call, but anything returning a future works,
/// which is what makes an `Action` testable without a network.
pub fn create_action<I, O, F, Fut, E>(f: F) -> Action<I, O>
where
    I: 'static,
    O: Clone + 'static,
    F: Fn(I) -> Fut + 'static,
    Fut: Future<Output = Result<O, E>> + 'static,
    E: std::fmt::Display + 'static,
{
    let (pending, set_pending) = create_signal(false);
    let (value, set_value) = create_signal(None);
    let (error, set_error) = create_signal(None);

    Action {
        pending,
        value,
        error,
        set_pending,
        set_value,
        set_error,
        run: flatten_display_errors(f),
        generation: Rc::new(Cell::new(0)),
    }
}

/// A future factory: what [`run_guarded`] runs and [`flatten_display_errors`]
/// produces. `Action` stores one as `run`, `Resource` as `fetcher`.
pub(crate) type GuardedFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>>>>;

/// Wrap an async operation so its error type is flattened to a `String` through
/// `Display`, keeping the error type out of every signature that mentions the
/// [`Action`] or [`Resource`](crate::resource::Resource) holding it.
/// `ServerFnError` already renders usefully through `Display`.
pub(crate) fn flatten_display_errors<I, T, F, Fut, E>(f: F) -> Rc<dyn Fn(I) -> GuardedFuture<T>>
where
    F: Fn(I) -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
    E: std::fmt::Display + 'static,
{
    Rc::new(move |input| {
        let fut = f(input);
        Box::pin(async move { fut.await.map_err(|e| e.to_string()) })
    })
}

/// The generation-guarded run shared by [`Action::dispatch`] and
/// [`Resource::fetch`](crate::resource::Resource): bump the generation, build
/// the future, apply the start-of-run writes, spawn the future, and discard
/// its result if a newer run superseded it before completion.
///
/// Off the browser there is no executor, so the operation would never be
/// polled. Returning *first* is the whole point: were `begin`'s writes to run
/// anyway, `pending` would latch true for the lifetime of the render and every
/// `disabled={pending}` button in the server-rendered markup would ship
/// disabled. The unrun closures are dropped, input and all, before any signal
/// is touched.
///
/// `apply` receives the completed outcome inside one [`batch`], so consumers
/// reading several of the completion signals see a single consistent
/// transition.
pub(crate) fn run_guarded<T: 'static>(
    generation: &Rc<Cell<u64>>,
    begin: impl FnOnce(),
    make_future: impl FnOnce() -> GuardedFuture<T>,
    apply: impl FnOnce(Result<T, String>) + 'static,
) {
    if !DISPATCH_HAS_EXECUTOR {
        return;
    }

    run_on_executor(generation, begin, make_future, apply);
}

/// The body of [`run_guarded`] past the executor check, separated so its
/// ordering contract is natively testable (natively `run_guarded` itself
/// returns at the check before reaching any of this).
///
/// Two panic-robustness guarantees live here:
///
/// 1. The future is constructed **before** `begin`'s writes. A future factory
///    that panics therefore unwinds before `pending` is set — the old order
///    latched `pending` true forever, because the future that was supposed to
///    clear it was never created.
/// 2. The spawned future is polled through [`CatchUnwind`], so a panic while
///    the operation runs resolves to the `Err` outcome — clearing `pending`
///    and populating `error` — instead of unwinding through the executor with
///    `pending` latched.
fn run_on_executor<T: 'static>(
    generation: &Rc<Cell<u64>>,
    begin: impl FnOnce(),
    make_future: impl FnOnce() -> GuardedFuture<T>,
    apply: impl FnOnce(Result<T, String>) + 'static,
) {
    let current = generation.get().wrapping_add(1);
    generation.set(current);

    let future = make_future();

    begin();

    let expected = generation.clone();

    spawn_local_task(async move {
        let outcome = CatchUnwind { inner: future }.await;

        // A newer run has started; this result is stale.
        if expected.get() != current {
            return;
        }

        batch(|| apply(outcome));
    });
}

/// Awaits the wrapped operation, converting a panic inside its `poll` into the
/// `Err` outcome instead of letting it unwind through the executor.
///
/// Hand-rolled: the `futures` crate — whose `FutureExt::catch_unwind` is the
/// off-the-shelf version of this — is not in the dependency tree, and a
/// one-struct wrapper does not justify adding it. `GuardedFuture` is
/// `Pin<Box<dyn Future>>`, which is `Unpin`, so no pin projection is needed.
///
/// On `wasm32-unknown-unknown` panics currently abort rather than unwind, so
/// the catch is best-effort there; it is exact on unwinding targets and keeps
/// both compilations on one code path.
struct CatchUnwind<T> {
    inner: GuardedFuture<T>,
}

impl<T> Future for CatchUnwind<T> {
    type Output = Result<T, String>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let inner = &mut self.get_mut().inner;
        // AssertUnwindSafe: on a caught panic the inner future is never polled
        // again — `Ready` is returned and the executor drops the task — so no
        // broken invariant can be observed.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.as_mut().poll(cx)));
        match result {
            Ok(poll) => poll,
            Err(payload) => std::task::Poll::Ready(Err(panic_message(payload))),
        }
    }
}

/// A human-readable message from a panic payload, for the action's `error`
/// signal.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "operation panicked".to_string()
    }
}

/// Whether this target has an executor [`Action::dispatch`] can hand a future to.
///
/// Only the browser does. A `!Send` future holding `Rc`-based signals has no
/// honest native equivalent — a Tokio runtime will not take it — and there is
/// nothing to dispatch *from* during server-side rendering anyway.
#[cfg(target_arch = "wasm32")]
pub(crate) const DISPATCH_HAS_EXECUTOR: bool = true;

/// See the wasm32 variant above.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DISPATCH_HAS_EXECUTOR: bool = false;

/// Run a future on the local task queue.
///
/// Natively this is unreachable — [`run_guarded`] returns at
/// [`DISPATCH_HAS_EXECUTOR`] before getting here — but the body is compiled on
/// both targets so the two paths cannot drift apart silently. Dropping is the
/// safe fallback if that early return is ever removed: it loses the operation,
/// where polling on a runtime that rejects `!Send` futures would not compile and
/// a `panic!` would turn a caller's mistake into a downed server.
#[cfg(target_arch = "wasm32")]
pub(crate) fn spawn_local_task<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(future);
}

/// See the wasm32 variant above.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_local_task<F>(future: F)
where
    F: Future<Output = ()> + 'static,
{
    drop(future);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The native contract, and the reason [`Action`] lives in `krab_core` at
    /// all: an island body compiles for both targets, so `dispatch` has to be
    /// callable — and harmless — during server-side rendering.
    ///
    /// This asserts the *absence* of the latch bug: an early version set
    /// `pending` before discovering it had nowhere to run the future, so every
    /// SSR pass that reached a dispatch rendered a permanently-disabled button.
    #[test]
    fn dispatch_is_inert_during_server_side_rendering() {
        let action = create_action(|input: u32| async move { Ok::<_, String>(input * 2) });

        for _ in 0..3 {
            action.dispatch(21);
        }

        assert!(
            !action.pending().get(),
            "dispatch must not latch pending where there is no executor to clear it"
        );
        assert_eq!(action.value().get(), None);
        assert_eq!(action.error().get(), None);
    }

    /// The signals are real signals natively, not stubs — markup that reads
    /// `pending` or `error` renders the same way it will after hydration.
    #[test]
    fn the_signals_are_readable_and_idle_before_any_dispatch() {
        let action = create_action(|_: ()| async move { Ok::<_, String>(1u32) });

        assert!(!action.pending().get());
        assert_eq!(action.value().get(), None);
        assert_eq!(action.error().get(), None);
    }

    /// Clones share state on both targets, so an SSR pass that clones an action
    /// into markup does not silently get a second, independent one.
    #[test]
    fn clones_share_the_same_signals_natively() {
        let action = create_action(|_: ()| async move { Ok::<_, String>(1u32) });
        let observer = action.clone();

        action.dispatch(());

        assert!(!observer.pending().get());
        assert_eq!(observer.value().get(), None);
    }

    /// The constructor-latch bug, exercised on the executor path
    /// (`run_on_executor`, which `run_guarded` reaches only on wasm): the
    /// future is built *before* `begin`, so a factory that panics unwinds
    /// before any signal write — with the old order `begin` had already set
    /// `pending` and nothing was left to clear it.
    ///
    /// The full wasm runtime behaviour (a real `dispatch` through
    /// `wasm_bindgen_futures`) cannot be exercised natively; this pins the
    /// shared ordering both targets compile.
    #[test]
    fn a_panicking_future_factory_does_not_reach_begin() {
        let generation = Rc::new(Cell::new(0u64));
        let began = Rc::new(Cell::new(false));

        let began_probe = began.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_on_executor(
                &generation,
                move || began_probe.set(true),
                || -> GuardedFuture<u32> { panic!("building the operation failed") },
                |_outcome| {},
            );
        }));

        assert!(panicked.is_err(), "the factory panic must propagate");
        assert!(
            !began.get(),
            "begin must not run when the factory panics: pending would latch \
             with no future left to clear it"
        );
    }

    /// A panic while the operation itself runs resolves to `Err` through
    /// [`CatchUnwind`] instead of unwinding through the executor — on wasm
    /// that is what clears `pending` and populates `error`.
    #[test]
    fn a_panicking_operation_resolves_to_the_error_outcome() {
        let future: GuardedFuture<u32> = Box::pin(async { panic!("operation failed mid-flight") });

        let outcome = tokio_test::block_on(CatchUnwind { inner: future });

        match outcome {
            Err(message) => assert!(
                message.contains("operation failed mid-flight"),
                "the panic message must reach the error state, got: {message}"
            ),
            Ok(_) => panic!("a panicking operation must resolve to Err"),
        }
    }
}
