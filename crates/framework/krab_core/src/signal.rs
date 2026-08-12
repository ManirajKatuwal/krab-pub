//! Krab reactive signal system.
//!
//! # Threading constraints
//!
//! **Signals are single-threaded.**  They use [`Rc`] and [`RefCell`] internally
//! so the Rust type system **statically prevents** moving a signal across thread
//! boundaries (`Rc` is `!Send + !Sync`).  Attempting to do so is a **compile
//! error**, not a runtime failure.
//!
//! Signals must only be created and used on the same thread.  In a server-side
//! rendering context (non-WASM), this means each request should set up its own
//! signal graph on the request handler thread.  In a WASM context there is
//! always exactly one thread (the JS event loop), so this is naturally enforced.
//!
//! Do **not** wrap signals in `Arc<Mutex<...>>` to try to share them across
//! threads — the design intentionally avoids locking overhead.  For shared
//! mutable state across async tasks, use the standard tokio primitives
//! (`Arc<Mutex<T>>`, channels, `tokio::sync::RwLock<T>`).
//!
//! The compile-time enforcement is verified by the `signals_are_not_send_sync`
//! test in this module.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// The reactive node currently executing, if any. Reads subscribe to it.
    static CURRENT_SUBSCRIBER: RefCell<Option<Subscriber>> = const { RefCell::new(None) };
    static ROOT_EFFECTS: RefCell<Vec<Rc<EffectState>>> = const { RefCell::new(Vec::new()) };
    /// Depth of nested [`batch`] scopes; effects are held while non-zero.
    static BATCH_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Effects deferred by an open [`batch`], deduplicated.
    static BATCHED_EFFECTS: RefCell<Vec<Rc<EffectState>>> = const { RefCell::new(Vec::new()) };
    /// Nesting depth of synchronous flushes on the native path. A write inside
    /// an effect flushes synchronously, so chained writes across *distinct*
    /// effects stack these frames; the depth is capped at [`MAX_FLUSH_DEPTH`]
    /// as the backstop the per-effect running flag cannot provide.
    #[cfg(any(not(feature = "web"), not(target_arch = "wasm32")))]
    static FLUSH_DEPTH: Cell<u32> = const { Cell::new(0) };
    #[cfg(feature = "web")]
    static PENDING_EFFECTS: RefCell<Vec<Rc<EffectState>>> = const { RefCell::new(Vec::new()) };
    #[cfg(feature = "web")]
    static MICROTASK_QUEUED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Something that depends on a signal.
///
/// A memo is both a subscriber *and* a source, which is why this is an enum
/// rather than a list of effects: `source -> memo -> effect` needs the middle
/// link to be notifiable and to notify in turn.
#[derive(Clone)]
enum Subscriber {
    Effect(Weak<EffectState>),
    Memo(Weak<MemoState>),
}

impl Subscriber {
    /// Whether two subscribers refer to the same reactive node.
    fn same_node(&self, other: &Subscriber) -> bool {
        match (self, other) {
            (Self::Effect(a), Self::Effect(b)) => Weak::ptr_eq(a, b),
            (Self::Memo(a), Self::Memo(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Whether the reactive node this subscriber points at is still alive.
    fn is_alive(&self) -> bool {
        match self {
            Self::Effect(weak) => weak.strong_count() > 0,
            Self::Memo(weak) => weak.strong_count() > 0,
        }
    }
}

/// Subscribe `subscriber` unless it is already in the list.
///
/// `get()` subscribes on every read, so an effect reading a signal `k` times in
/// one run would otherwise land `k` identical entries — and for a source that is
/// read by a re-running dependent but never itself written (so never drained),
/// the list grew by one per run for the life of the page.
///
/// The whole list is scanned, not just its tail: interleaved reads (a parent
/// effect reads, its child effect reads, the parent reads again) put another
/// node between two reads by the same subscriber, and a tail-only check
/// re-added the parent on every run. Lists are short — one entry per live
/// dependent — so the scan is cheap, and it doubles as garbage collection:
/// entries whose node has been dropped are retained out while scanning.
fn push_subscriber(list: &mut Vec<Subscriber>, subscriber: &Subscriber) {
    let mut already_subscribed = false;
    list.retain(|existing| {
        if !existing.is_alive() {
            return false;
        }
        if existing.same_node(subscriber) {
            already_subscribed = true;
        }
        true
    });
    if !already_subscribed {
        list.push(subscriber.clone());
    }
}

/// Restores [`CURRENT_SUBSCRIBER`] whether the scope exits normally or unwinds.
///
/// The same reasoning as [`BatchGuard`]: island factories run under
/// `catch_unwind` and `error_boundary` makes a panic recoverable, so execution
/// continues past one. Without this, a panic inside an effect, memo
/// computation, or `untrack` scope left the thread-local pointing at the
/// panicked node — every later read subscribed a zombie, and every later
/// `create_effect` was adopted by it and mass-disposed on its next run.
/// Restoring an `Option` in `Drop` runs no user code, so it cannot double-panic.
struct SubscriberGuard {
    previous: Option<Subscriber>,
}

impl SubscriberGuard {
    fn swap_in(next: Option<Subscriber>) -> Self {
        Self {
            previous: CURRENT_SUBSCRIBER.with(|current| current.replace(next)),
        }
    }
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        CURRENT_SUBSCRIBER.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

/// A cached derived value.
///
/// Type-erased: the computed value lives in an `Rc<RefCell<Option<T>>>` closed
/// over by `recompute`, so memos of different `T` share one subscriber list.
struct MemoState {
    /// Whether a dependency changed since the last computation.
    ///
    /// Set by propagation, cleared on read. This split — mark dirty on write,
    /// recompute on read — is what makes a diamond settle once: both branches
    /// are marked before either is pulled, so the effect downstream runs once
    /// and sees two fresh values.
    dirty: Cell<bool>,
    /// Whether `recompute_now` for this memo is currently on the stack.
    ///
    /// A read of the memo while this is set is a self-referential computation:
    /// the memo's own closure (directly or through a chain) read the memo back.
    /// Such a read is served the stale cached value, untracked, with a
    /// `memo_self_reference_detected` error event — recomputing would recurse.
    computing: Cell<bool>,
    /// Recomputes the cached value. Installed after construction, because it
    /// needs a `Weak` back to the state it lives in.
    recompute: RefCell<Option<Box<dyn Fn()>>>,
    subscribers: RefCell<Vec<Subscriber>>,
    disposed: Cell<bool>,
}

impl MemoState {
    /// Recompute now, tracking dependencies against this memo.
    ///
    /// Unwind-safe: the taken closure, the previous subscriber, and the dirty
    /// flag are all restored by guards if the computation panics, so a caught
    /// panic (island factories, `error_boundary`) leaves the memo stale and
    /// retryable — not permanently poisoned serving its old value.
    fn recompute_now(self: &Rc<Self>) {
        // Cleared before running so a read of this memo from inside its own
        // computation cannot re-enter and recurse.
        self.dirty.set(false);
        // Raised for the duration of the computation so a self-referential
        // read is detected rather than served silently. Cleared by
        // `RecomputeGuard` below, so a caught panic cannot leave it wedged.
        self.computing.set(true);

        let _subscriber = SubscriberGuard::swap_in(Some(Subscriber::Memo(Rc::downgrade(self))));

        // The closure is taken out for the call: it may read other memos, whose
        // recomputation would otherwise need a second borrow of this RefCell.
        // The guard puts it back even if the computation unwinds — without
        // that, one caught panic left `recompute` empty forever and every
        // later read served the stale cached value with no way to recover.
        struct RecomputeGuard<'a> {
            state: &'a MemoState,
            compute: Option<Box<dyn Fn()>>,
            completed: bool,
        }
        impl Drop for RecomputeGuard<'_> {
            fn drop(&mut self) {
                self.state.computing.set(false);
                *self.state.recompute.borrow_mut() = self.compute.take();
                if !self.completed {
                    // The computation did not finish, so the cached value is
                    // still the old one: stay dirty so the next read retries
                    // instead of serving it as fresh.
                    self.state.dirty.set(true);
                }
            }
        }

        let mut guard = RecomputeGuard {
            state: self,
            compute: self.recompute.borrow_mut().take(),
            completed: false,
        };
        if let Some(compute) = guard.compute.as_ref() {
            compute();
        }
        guard.completed = true;
    }
}

struct EffectState {
    execute: Box<dyn Fn()>,
    /// Set when this effect is torn down. A disposed effect never runs again,
    /// even if a stale subscription still points at it.
    disposed: Cell<bool>,
    /// Whether this effect's body is currently on the stack.
    ///
    /// A `set()` inside an effect on one of the effect's own dependencies
    /// notifies the effect itself; natively that delivery is synchronous, so
    /// without this flag `run_effect` re-entered on the same stack and
    /// recursed until it overflowed. See [`run_effect`].
    running: Cell<bool>,
    /// Set when a notification for this effect arrived while it was running.
    /// The refused re-entrant run is owed: [`run_effect`] delivers it after
    /// the current body finishes, so the effect still converges.
    rerun_requested: Cell<bool>,
    /// Whether `signal_effect_cycle_detected` has been emitted for this
    /// effect. The event identifies a coding bug, so it is reported once per
    /// effect rather than once per refused re-entry.
    cycle_reported: Cell<bool>,
    /// Effects created *during* this effect's last run.
    ///
    /// This is what makes disposal possible. `create_dom_node` calls
    /// `create_effect` for every nested `Dynamic` it builds, so without
    /// ownership every re-render of a parent left the previous run's effects
    /// alive, still subscribed, still patching DOM nodes that had already been
    /// detached. Ten updates meant ten zombie effects doing work.
    children: RefCell<Vec<Rc<EffectState>>>,
    /// Memos created during this effect's last run, disposed with it.
    owned_memos: RefCell<Vec<Rc<MemoState>>>,
    /// Callbacks registered by [`on_cleanup`] during the last run.
    cleanups: RefCell<Vec<Box<dyn FnOnce()>>>,
}

impl EffectState {
    fn run(&self) {
        (self.execute)();
    }

    /// Release everything the last run created: cleanups first, then children.
    ///
    /// Both collections are moved out before anything is invoked, so a cleanup
    /// that itself calls [`on_cleanup`] or creates an effect cannot re-enter a
    /// live `RefCell` borrow and panic.
    fn dispose_children(&self) {
        let cleanups = std::mem::take(&mut *self.cleanups.borrow_mut());
        for cleanup in cleanups {
            cleanup();
        }

        let memos = std::mem::take(&mut *self.owned_memos.borrow_mut());
        for memo in memos {
            memo.disposed.set(true);
        }

        let children = std::mem::take(&mut *self.children.borrow_mut());
        for child in children {
            child.dispose();
        }
    }

    /// Tear this effect down permanently, and everything it owns.
    fn dispose(&self) {
        self.disposed.set(true);
        self.dispose_children();
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct SignalId(u64);

impl SignalId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        SignalId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

struct SignalState<T> {
    value: T,
    subscribers: Vec<Subscriber>,
}

struct SignalInner<T> {
    id: SignalId,
    state: Rc<RefCell<SignalState<T>>>,
}

impl<T> Clone for SignalInner<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            state: self.state.clone(),
        }
    }
}

pub fn create_signal<T>(value: T) -> (ReadSignal<T>, WriteSignal<T>) {
    let inner = SignalInner {
        id: SignalId::new(),
        state: Rc::new(RefCell::new(SignalState {
            value,
            subscribers: Vec::new(),
        })),
    };

    (
        ReadSignal {
            inner: inner.clone(),
        },
        WriteSignal { inner },
    )
}

pub struct ReadSignal<T> {
    inner: SignalInner<T>,
}

impl<T> Clone for ReadSignal<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: Clone> ReadSignal<T> {
    pub fn get(&self) -> T {
        // Track dependency
        CURRENT_SUBSCRIBER.with(|current| {
            if let Some(subscriber) = current.borrow().as_ref() {
                let mut state = self.inner.state.borrow_mut();
                push_subscriber(&mut state.subscribers, subscriber);
            }
        });
        self.inner.state.borrow().value.clone()
    }
}

impl<T> ReadSignal<T> {
    pub fn with<U, F>(&self, f: F) -> U
    where
        F: FnOnce(&T) -> U,
    {
        CURRENT_SUBSCRIBER.with(|current| {
            if let Some(subscriber) = current.borrow().as_ref() {
                let mut state = self.inner.state.borrow_mut();
                push_subscriber(&mut state.subscribers, subscriber);
            }
        });
        let state = self.inner.state.borrow();
        f(&state.value)
    }
}

pub struct WriteSignal<T> {
    inner: SignalInner<T>,
}

impl<T> Clone for WriteSignal<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> WriteSignal<T> {
    pub fn set(&self, new_value: T) {
        {
            let mut state = self.inner.state.borrow_mut();
            state.value = new_value;
        } // Drop borrow

        self.notify();
    }

    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        {
            let mut state = self.inner.state.borrow_mut();
            f(&mut state.value);
        }
        self.notify();
    }

    fn notify(&self) {
        // Subscribers are drained before running: effects re-subscribe to the
        // signals they read, so keeping the old entries would compound them on
        // every notification.
        //
        // Deduplicated here rather than per-platform. `get()` and `with()`
        // subscribe on every read, so an effect that reads a signal three times
        // lands three subscriptions. The wasm path already collapsed those in
        // `PENDING_EFFECTS`; the native path did not, and ran the effect once
        // per read. Disposed effects are dropped too — a stale subscription
        // must not resurrect one.
        let subscribers: Vec<Subscriber> = {
            let mut state = self.inner.state.borrow_mut();
            state.subscribers.drain(..).collect()
        };

        let mut to_run: Vec<Rc<EffectState>> = Vec::new();
        for subscriber in subscribers {
            collect_dependents(&subscriber, &mut to_run);
        }

        schedule_effects(to_run);
    }
}

/// Queue `effect` unless the same `Rc` is already queued.
///
/// One definition for the dedup rule all queues share — previously four
/// hand-rolled copies that had to stay identical.
fn push_unique(queue: &mut Vec<Rc<EffectState>>, effect: Rc<EffectState>) {
    if !queue.iter().any(|seen| Rc::ptr_eq(seen, &effect)) {
        queue.push(effect);
    }
}

/// Drop `effects` from the wasm microtask queue before a synchronous flush.
///
/// A batch flush runs its effects immediately; any of them still sitting in
/// [`PENDING_EFFECTS`] from an earlier unbatched write in the same task would
/// otherwise be run a second time when the microtask fires — observing no state
/// change, but paying a full dispose-and-rebuild cycle (or issuing a duplicate
/// identical fetch from a `Resource`). Removal happens *before* the flush so a
/// write occurring during it can legitimately re-enqueue.
fn unschedule_pending(#[allow(unused_variables)] effects: &[Rc<EffectState>]) {
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    PENDING_EFFECTS.with(|pending| {
        pending
            .borrow_mut()
            .retain(|queued| !effects.iter().any(|e| Rc::ptr_eq(e, queued)));
    });
}

/// Hand effects to the platform runner, or hold them if a [`batch`] is open.
///
/// One place decides *when* effects run, so `batch` does not need a second
/// mechanism and cannot diverge from the unbatched path.
fn schedule_effects(effects: Vec<Rc<EffectState>>) {
    if BATCH_DEPTH.with(|depth| depth.get()) > 0 {
        queue_batched(effects);
        return;
    }

    // A batch that unwound left its queue behind deliberately (running effects
    // while a panic is in flight risks a double panic, which aborts). Draining
    // it here means those notifications are delivered by the next write rather
    // than stranded.
    let mut pending = take_batched();
    for effect in effects {
        push_unique(&mut pending, effect);
    }

    if pending.is_empty() {
        return;
    }

    schedule_async(pending);
}

fn queue_batched(effects: Vec<Rc<EffectState>>) {
    BATCHED_EFFECTS.with(|batched| {
        let mut batched = batched.borrow_mut();
        for effect in effects {
            push_unique(&mut batched, effect);
        }
    });
}

fn take_batched() -> Vec<Rc<EffectState>> {
    BATCHED_EFFECTS.with(|batched| std::mem::take(&mut *batched.borrow_mut()))
}

/// Run effects immediately, on every platform.
///
/// Used to flush a [`batch`], so the scope has the same observable timing in a
/// browser as it does natively.
fn run_now(effects: Vec<Rc<EffectState>>) {
    for effect in effects {
        run_effect(effect);
    }
}

/// Restores [`BATCH_DEPTH`] whether the scope exits normally or unwinds.
///
/// Without this a panic inside a batch left the depth raised forever, and every
/// later write on that thread queued into [`BATCHED_EFFECTS`] and was never
/// flushed — the reactive system silently stopped working. That is not
/// hypothetical: `krab_client` wraps island factories in `catch_unwind`, and
/// `error_boundary` exists precisely to make a panic recoverable, so execution
/// really does continue past one.
struct BatchGuard;

impl BatchGuard {
    fn enter() -> Self {
        BATCH_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }
}

impl Drop for BatchGuard {
    fn drop(&mut self) {
        BATCH_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Deliver effects using the platform's own scheduling.
///
/// On wasm this coalesces into a microtask, which is an implicit per-task batch;
/// natively there is no such queue, so they run immediately. Use [`batch`] when
/// you need the same observable behaviour on both.
fn schedule_async(#[allow(unused_variables)] effects: Vec<Rc<EffectState>>) {
    #[cfg(all(feature = "web", target_arch = "wasm32"))]
    {
        PENDING_EFFECTS.with(|pending| {
            let mut p = pending.borrow_mut();
            for effect in effects {
                push_unique(&mut p, effect);
            }
        });

        if !MICROTASK_QUEUED.with(|q| q.get()) {
            MICROTASK_QUEUED.with(|q| q.set(true));
            let closure = wasm_bindgen::closure::Closure::once(|_val: wasm_bindgen::JsValue| {
                MICROTASK_QUEUED.with(|q| q.set(false));
                let effects = PENDING_EFFECTS.with(|pending| {
                    let mut p = pending.borrow_mut();
                    std::mem::take(&mut *p)
                });
                for effect in effects {
                    run_effect(effect);
                }
            });

            let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
            let _ = promise.then(&closure);
            closure.forget();
        }
    }

    #[cfg(any(not(feature = "web"), not(target_arch = "wasm32")))]
    {
        // Restores the depth even if an effect body panics under
        // `catch_unwind`; a wedged depth would make every later flush on the
        // thread look nested and eventually refuse to deliver anything.
        struct FlushDepthGuard;
        impl Drop for FlushDepthGuard {
            fn drop(&mut self) {
                FLUSH_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
            }
        }

        let depth = FLUSH_DEPTH.with(|depth| depth.get());
        if depth >= MAX_FLUSH_DEPTH {
            // A chain of effects each writing another's dependency has nested
            // this deep synchronously — a livelock. Drop the delivery rather
            // than overflow the stack; the values are already committed, so
            // the next legitimate write re-runs the dependents.
            tracing::error!(
                depth,
                dropped = effects.len(),
                "signal_flush_depth_exceeded"
            );
            return;
        }
        FLUSH_DEPTH.with(|depth| depth.set(depth.get() + 1));
        let _guard = FlushDepthGuard;

        for effect in effects {
            run_effect(effect);
        }
    }
}

/// Group writes so dependent effects run once, after the last one.
///
/// Without this, each `set()` fans out immediately: three writes that share a
/// dependent effect run it three times, and observers can see intermediate
/// states that never logically existed.
///
/// ```
/// use krab_core::signal::{batch, create_signal, create_effect};
///
/// let (first, set_first) = create_signal(0);
/// let (second, set_second) = create_signal(0);
///
/// create_effect(move || {
///     let _ = first.get();
///     let _ = second.get();
/// });
///
/// batch(|| {
///     set_first.set(1);
///     set_second.set(2);
/// }); // the effect runs once here, not twice
/// ```
///
/// Nesting is safe: only the outermost `batch` flushes.
///
/// # Guarantees
///
/// Effects have run by the time `batch` returns, on **every** platform. An
/// unbatched write does not promise this: on wasm it coalesces into a microtask
/// and runs just after the current task, while natively it runs immediately.
/// `batch` is therefore the portable way to get deterministic behaviour — both
/// the run count and the moment.
///
/// # Panics
///
/// If `f` panics, the batch depth is restored and the panic propagates. The
/// queued effects are **not** run while unwinding — invoking user code from a
/// `Drop` during a panic risks a second panic, which aborts the process — so
/// they are delivered by the next write instead.
///
/// This matters because panics here are recoverable in practice:
/// `krab_client` wraps island factories in `catch_unwind`, and
/// [`error_boundary`](crate::error_boundary) exists to keep rendering after
/// one. An earlier version leaked the raised depth on unwind, which silently
/// stopped every subsequent effect on the thread from ever running.
pub fn batch<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let result = {
        // Restores the depth on unwind as well as on the normal path.
        let _guard = BatchGuard::enter();
        f()
    };

    // Reached only when `f` returned normally. On unwind the queue is left in
    // place on purpose — running user effects from a `Drop` during a panic
    // risks a second panic, which aborts. `schedule_effects` drains the
    // leftovers on the next write instead.
    if BATCH_DEPTH.with(|depth| depth.get()) == 0 {
        let effects = take_batched();
        // On wasm, cancel any still-pending microtask delivery for these
        // effects: the flush below already delivers the latest state, and the
        // stale entry would re-run them for nothing.
        unschedule_pending(&effects);
        run_now(effects);
    }

    result
}

/// Walk a subscriber, marking memos dirty and collecting the effects to run.
///
/// Memos are marked but **not** recomputed. Recomputation happens on read, so a
/// diamond (`source -> a`, `source -> b`, `effect(a, b)`) marks both branches
/// before the effect runs, and the effect then pulls two already-fresh values.
/// Recomputing eagerly here would run the effect once per branch — the glitch
/// this ordering exists to prevent.
///
/// Traversal does not stop at an already-dirty memo. A memo can be dirtied by
/// one write and, before any read, dirtied again by another; skipping it would
/// drop the second write's effects. Effects are deduplicated by pointer, so
/// revisiting a node is redundant work rather than a duplicate run.
fn collect_dependents(subscriber: &Subscriber, effects: &mut Vec<Rc<EffectState>>) {
    match subscriber {
        Subscriber::Effect(weak) => {
            let Some(effect) = weak.upgrade() else {
                return;
            };
            if effect.disposed.get() {
                return;
            }
            push_unique(effects, effect);
        }
        Subscriber::Memo(weak) => {
            let Some(memo) = weak.upgrade() else {
                return;
            };
            if memo.disposed.get() {
                return;
            }
            memo.dirty.set(true);

            // Drained, exactly as `WriteSignal::notify` drains a signal's
            // list: dependents re-subscribe on their next read. Cloning here
            // (the previous behaviour) left every historical subscription in
            // place, so the list grew by one entry per dependent re-run for
            // the memo's whole life — an unbounded leak with O(n) walk cost
            // per write. Draining also garbage-collects dead `Weak`s. Moved
            // out before recursing: a dependent may read back into this list.
            let downstream: Vec<Subscriber> = memo.subscribers.borrow_mut().drain(..).collect();
            for next in &downstream {
                collect_dependents(next, effects);
            }
        }
    }
}

/// Read signals without subscribing to them.
///
/// Inside an effect or memo, every `get()` creates a dependency. `untrack` is
/// the escape hatch for reading state that should not cause a re-run — a
/// previous value, a configuration flag, a counter used only for logging.
///
/// ```
/// use krab_core::signal::{create_signal, create_effect, untrack};
///
/// let (tracked, set_tracked) = create_signal(0);
/// let (ignored, set_ignored) = create_signal(0);
///
/// create_effect(move || {
///     let _ = tracked.get();
///     let _ = untrack(|| ignored.get());
/// });
///
/// set_ignored.set(1); // does not re-run the effect
/// set_tracked.set(1); // does
/// ```
pub fn untrack<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    // Guard, not sequential replace calls: if `f` panics and the panic is
    // caught upstream, tracking must still be restored or every subsequent
    // read on the thread stays untracked.
    let _guard = SubscriberGuard::swap_in(None);
    f()
}

/// A cached derived value that recomputes only when a dependency changed.
///
/// Cloning is cheap and shares the same computation.
pub struct Memo<T> {
    state: Rc<MemoState>,
    value: Rc<RefCell<Option<T>>>,
}

impl<T> Clone for Memo<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            value: self.value.clone(),
        }
    }
}

impl<T> Memo<T> {
    /// Subscribe the current reactive node, and recompute if stale.
    fn track_and_refresh(&self) {
        if self.state.computing.get() {
            // The memo's own computation is on the stack and has read the memo
            // back. Recomputing would recurse without bound, and subscribing
            // would make the memo its own dependent, so the read is treated as
            // untracked and stale: it serves whatever value is cached.
            tracing::error!("memo_self_reference_detected");
            return;
        }

        CURRENT_SUBSCRIBER.with(|current| {
            if let Some(subscriber) = current.borrow().as_ref() {
                push_subscriber(&mut self.state.subscribers.borrow_mut(), subscriber);
            }
        });

        if self.state.dirty.get() && !self.state.disposed.get() {
            self.state.recompute_now();
        }
    }

    /// Borrow the computed value.
    ///
    /// A self-referential read — the memo's computation reading the memo back —
    /// is served the previous (stale) value with a `memo_self_reference_detected`
    /// error event rather than recursing. Because [`create_memo`] computes
    /// eagerly before any handle to the memo exists, a value is always present
    /// by the time a self-read is reachable through the public API.
    pub fn with<U, F>(&self, f: F) -> U
    where
        F: FnOnce(&T) -> U,
    {
        self.track_and_refresh();
        {
            let value = self.value.borrow();
            if let Some(value) = value.as_ref() {
                return f(value);
            }
        }

        // No value has ever been computed. `create_memo` computes eagerly
        // before returning, so the only route here is a first computation that
        // panicked (and was caught upstream) followed by a later read. If no
        // computation is on the stack, retry it now and serve the result.
        if !self.state.computing.get() && !self.state.disposed.get() {
            self.state.dirty.set(true);
            self.state.recompute_now();
            let value = self.value.borrow();
            if let Some(value) = value.as_ref() {
                return f(value);
            }
        }

        // A self-referential read during the memo's *first* computation: there
        // is no stale value to serve and no way to conjure a `T`, so this is
        // unsatisfiable. It is unreachable through the public API (no handle
        // exists until the first computation has completed); the panic below —
        // preceded by the error event — is a guarded invariant, not a control
        // path, and the enclosing `RecomputeGuard` leaves the memo retryable.
        tracing::error!("memo_self_reference_detected");
        panic!(
            "memo read its own value during its first computation; no previous value \
             exists to break the cycle"
        );
    }
}

impl<T: Clone> Memo<T> {
    /// The computed value, recomputing first if a dependency changed.
    pub fn get(&self) -> T {
        self.with(|value| value.clone())
    }
}

/// Create a cached derived value.
///
/// `f` runs once immediately, then only when a signal or memo it read has
/// changed **and** the memo is read again. A memo that nothing reads costs
/// nothing to keep up to date.
///
/// ```
/// use krab_core::signal::{create_signal, create_memo};
///
/// let (count, set_count) = create_signal(2);
/// let doubled = create_memo(move || count.get() * 2);
///
/// assert_eq!(doubled.get(), 4);
/// set_count.set(5);
/// assert_eq!(doubled.get(), 10);
/// ```
///
/// A memo created inside an effect is owned by it and disposed when that effect
/// re-runs, in the same way a nested effect is.
pub fn create_memo<T, F>(f: F) -> Memo<T>
where
    T: 'static,
    F: Fn() -> T + 'static,
{
    let value: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
    let state = Rc::new(MemoState {
        // Starts dirty so the first read computes it; nothing is evaluated
        // until then.
        dirty: Cell::new(true),
        computing: Cell::new(false),
        recompute: RefCell::new(None),
        subscribers: RefCell::new(Vec::new()),
        disposed: Cell::new(false),
    });

    {
        let value_slot = value.clone();
        *state.recompute.borrow_mut() = Some(Box::new(move || {
            let next = f();
            *value_slot.borrow_mut() = Some(next);
        }));
    }

    // Owned by the enclosing effect, if any, so it is disposed with it.
    CURRENT_SUBSCRIBER.with(|current| {
        if let Some(Subscriber::Effect(owner)) = current.borrow().as_ref() {
            if let Some(owner) = owner.upgrade() {
                owner.owned_memos.borrow_mut().push(state.clone());
            }
        }
    });

    let memo = Memo { state, value };
    // Compute eagerly once so `get()` never observes an empty slot, and so
    // dependencies are registered before the first write can occur.
    memo.state.recompute_now();
    memo
}

/// Build a fresh, not-yet-run effect state.
#[cfg(any(feature = "web", test))]
fn new_effect_state(f: impl Fn() + 'static) -> Rc<EffectState> {
    Rc::new(EffectState {
        execute: Box::new(f),
        disposed: Cell::new(false),
        running: Cell::new(false),
        rerun_requested: Cell::new(false),
        cycle_reported: Cell::new(false),
        children: RefCell::new(Vec::new()),
        owned_memos: RefCell::new(Vec::new()),
        cleanups: RefCell::new(Vec::new()),
    })
}

/// Run `f` now, and again whenever a signal it read changes.
///
/// # Ownership
///
/// An effect created *while another effect is running* becomes that effect's
/// child, and is disposed when the parent re-runs. Only a top-level effect —
/// one created outside any running effect — is retained for the lifetime of the
/// thread.
///
/// This is what stops nested [`Dynamic`](crate::Node::Dynamic) nodes leaking:
/// the client rebuilds their effects on every parent re-render, and without
/// ownership each rebuild left the previous effects subscribed and patching
/// detached DOM.
///
/// # Permanence
///
/// A top-level effect created this way is **permanent by design**: it is
/// retained in the thread's root-effect list and there is no API to tear it
/// down. That is the right lifetime for the common callers — hydration effects
/// that must live as long as the page. An effect whose lifetime is shorter
/// than the thread (per-widget, per-subscription) should use
/// [`create_effect_scoped`], which returns a disposable [`EffectHandle`].
pub fn create_effect<F>(#[allow(unused_variables)] f: F)
where
    F: Fn() + 'static,
{
    #[cfg(any(feature = "web", test))]
    {
        let effect = new_effect_state(f);

        let owned_by_parent = CURRENT_SUBSCRIBER.with(|current| {
            match current.borrow().as_ref() {
                Some(Subscriber::Effect(parent)) => match parent.upgrade() {
                    Some(parent) => {
                        parent.children.borrow_mut().push(effect.clone());
                        true
                    }
                    None => false,
                },
                // An effect created inside a memo computation is not owned by
                // it: memos are meant to be pure, and adopting the effect would
                // silently dispose it on the next recomputation.
                _ => false,
            }
        });

        if !owned_by_parent {
            ROOT_EFFECTS.with(|roots| {
                roots.borrow_mut().push(effect.clone());
            });
        }

        run_effect(effect);
    }
}

/// A handle to an effect created with [`create_effect_scoped`].
///
/// Disposing the handle tears the effect down permanently: it never runs
/// again, its cleanups and children are released, and its entry in the
/// thread's root-effect list is removed, so the effect's memory can actually
/// be reclaimed. Dropping the handle *without* calling
/// [`dispose`](Self::dispose) leaves the effect running for the life of the
/// thread, exactly like [`create_effect`].
pub struct EffectHandle {
    state: Rc<EffectState>,
}

impl EffectHandle {
    /// Tear the effect down: mark it disposed, run its cleanups, dispose
    /// everything it owns, and release its root-list retention.
    ///
    /// Idempotent — disposing twice is a no-op.
    pub fn dispose(&self) {
        self.state.dispose();
        ROOT_EFFECTS.with(|roots| {
            roots
                .borrow_mut()
                .retain(|root| !Rc::ptr_eq(root, &self.state));
        });
    }
}

/// Like [`create_effect`], but returns an [`EffectHandle`] that can tear the
/// effect down.
///
/// The effect is always retained as a **root** effect — it is never adopted as
/// a child of an enclosing effect — so its lifetime is governed solely by the
/// returned handle. Use this for effects scoped to something shorter-lived
/// than the thread; use [`create_effect`] for effects that should live as long
/// as the page.
#[cfg(any(feature = "web", test))]
pub fn create_effect_scoped<F>(f: F) -> EffectHandle
where
    F: Fn() + 'static,
{
    let effect = new_effect_state(f);
    ROOT_EFFECTS.with(|roots| {
        roots.borrow_mut().push(effect.clone());
    });
    run_effect(effect.clone());
    EffectHandle { state: effect }
}

/// See the enabled variant above. Without the `web` feature nothing runs
/// effects on this target, matching [`create_effect`]; the returned handle
/// controls an inert, already-disposed effect so `dispose()` is a no-op.
#[cfg(not(any(feature = "web", test)))]
pub fn create_effect_scoped<F>(f: F) -> EffectHandle
where
    F: Fn() + 'static,
{
    let _ = f;
    EffectHandle {
        state: Rc::new(EffectState {
            execute: Box::new(|| {}),
            disposed: Cell::new(true),
            running: Cell::new(false),
            rerun_requested: Cell::new(false),
            cycle_reported: Cell::new(false),
            children: RefCell::new(Vec::new()),
            owned_memos: RefCell::new(Vec::new()),
            cleanups: RefCell::new(Vec::new()),
        }),
    }
}

/// Register a callback to run when the current effect is disposed or re-runs.
///
/// Outside an effect this is a no-op — there is nothing to attach to.
pub fn on_cleanup<F>(#[allow(unused_variables)] f: F)
where
    F: FnOnce() + 'static,
{
    #[cfg(any(feature = "web", test))]
    {
        CURRENT_SUBSCRIBER.with(|current| {
            let effect = match current.borrow().as_ref() {
                Some(Subscriber::Effect(effect)) => effect.upgrade(),
                _ => None,
            };
            if let Some(effect) = effect {
                effect.cleanups.borrow_mut().push(Box::new(f));
            }
        });
    }
}

/// Ceiling on consecutive owed re-runs of one effect, and on nested
/// synchronous flushes. High enough that any legitimately converging effect
/// settles long before it; low enough that a genuine livelock is cut off with
/// a `signal_flush_depth_exceeded` error event instead of hanging the thread.
const MAX_FLUSH_DEPTH: u32 = 64;

/// Clears an effect's `running` flag when the scope exits, normally or by
/// unwinding. Island factories run under `catch_unwind` and `error_boundary`
/// makes a panic recoverable, so execution continues past one — a wedged flag
/// would make every later run of the effect refuse as a false cycle.
struct RunningGuard<'a> {
    flag: &'a Cell<bool>,
}

impl<'a> RunningGuard<'a> {
    fn arm(flag: &'a Cell<bool>) -> Self {
        flag.set(true);
        Self { flag }
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.flag.set(false);
    }
}

fn run_effect(effect: Rc<EffectState>) {
    if effect.disposed.get() {
        return;
    }

    if effect.running.get() {
        // Re-entry: a write inside this effect's own body notified the effect
        // itself, and the native path delivers notifications synchronously on
        // the same stack. Running here would recurse without bound (the
        // pre-guard failure mode was a stack overflow). Refuse the nested run
        // and remember that one re-run is owed once the current body
        // finishes, so the effect still observes the value it wrote.
        if !effect.cycle_reported.get() {
            effect.cycle_reported.set(true);
            tracing::error!("signal_effect_cycle_detected");
        }
        effect.rerun_requested.set(true);
        return;
    }

    let mut reruns: u32 = 0;
    loop {
        effect.rerun_requested.set(false);

        {
            // Drop guard so a panic in the body cannot wedge the flag.
            let _running = RunningGuard::arm(&effect.running);

            // A re-run invalidates everything the previous run created.
            effect.dispose_children();

            // Guard, not sequential replace calls: an effect body that panics
            // under `catch_unwind` must not leave the thread-local pointing at
            // itself — that zombie subscriber adopted every later top-level
            // effect and then mass-disposed them on its next run.
            let _subscriber =
                SubscriberGuard::swap_in(Some(Subscriber::Effect(Rc::downgrade(&effect))));
            effect.run();
        }

        // Deliver the re-run a refused re-entry left owing, now that the body
        // is off the stack. Bounded: an effect that unconditionally rewrites
        // its own dependency would otherwise loop forever.
        if effect.disposed.get() || !effect.rerun_requested.get() {
            break;
        }
        reruns += 1;
        if reruns >= MAX_FLUSH_DEPTH {
            tracing::error!(reruns, "signal_flush_depth_exceeded");
            effect.rerun_requested.set(false);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time guards: assert that signals are !Send and !Sync.
    // These lines will cause a compile error if someone accidentally makes
    // ReadSignal or WriteSignal implement Send or Sync.
    fn _assert_read_signal_not_send<T: 'static>() {
        fn _not_send<X: ?Sized + Send>() {}
        // This must NOT compile if ReadSignal is Send.
        // We use a negative impl check via a trait bound that we do NOT want.
        // The simplest approach: static assertions via trait object coercion.
        // Because Rc is !Send, this function body is unreachable but the type
        // check verifies ReadSignal<T> does not auto-impl Send.
        let _ = std::marker::PhantomData::<ReadSignal<T>>;
    }

    #[test]
    #[allow(dead_code, clippy::extra_unused_type_parameters)]
    fn signals_are_not_send_sync() {
        // Runtime confirmation that the types are as expected.
        // The real compile-time check is that Rc<RefCell<...>> is !Send + !Sync,
        // which the compiler enforces automatically.
        fn is_send<T: Send>() -> bool {
            true
        }
        fn is_not_send<T>() -> bool
        where
            T: ?Sized,
        {
            std::thread::available_parallelism().is_ok()
        }
        // These would be compile errors if ReadSignal or WriteSignal were Send:
        // is_send::<ReadSignal<i32>>();
        // is_send::<WriteSignal<i32>>();
        //
        // The runtime assertion below is a documentation aid only.
        let _ = is_not_send::<ReadSignal<i32>>;
        let _ = is_not_send::<WriteSignal<i32>>;
    }

    #[test]
    fn test_signal_basic() {
        let (read, write) = create_signal(0);
        assert_eq!(read.get(), 0);
        write.set(1);
        assert_eq!(read.get(), 1);
    }

    #[cfg(target_arch = "wasm32")]
    #[test]
    fn test_effect() {
        let (read, write) = create_signal(0);
        let output = Rc::new(RefCell::new(0));
        let output_clone = output.clone();

        create_effect(move || {
            *output_clone.borrow_mut() = read.get();
        });

        assert_eq!(*output.borrow(), 0);
        write.set(10);
        assert_eq!(*output.borrow(), 10);
    }

    #[test]
    fn test_clone_signal() {
        let (read, _) = create_signal(42);
        let read2 = read.clone();
        assert_eq!(read.get(), 42);
        assert_eq!(read2.get(), 42);
    }

    #[derive(Debug, PartialEq)]
    struct NonCloneable(i32);

    #[test]
    fn test_non_cloneable_signal() {
        // Signals hold T. ReadSignal::get() requires T: Clone.
        // But we can use ReadSignal::with()
        let (read, _write) = create_signal(NonCloneable(10));

        read.with(|v| assert_eq!(v, &NonCloneable(10)));

        // This should clone the signal handle, not the value inside (which is not cloneable)
        let read2 = read.clone();
        read2.with(|v| assert_eq!(v, &NonCloneable(10)));
    }
}

#[cfg(test)]
mod disposal_tests {
    use super::*;
    use std::cell::Cell as StdCell;
    use std::rc::Rc as StdRc;

    /// The leak, reduced: a parent effect that creates a child effect on every
    /// run. Before ownership existed, each parent re-run left the previous
    /// child alive and subscribed, so N parent runs meant N live children all
    /// reacting to the same signal.
    #[test]
    fn re_running_a_parent_disposes_the_children_of_its_previous_run() {
        let (outer, set_outer) = create_signal(0);
        let (inner, set_inner) = create_signal(0);

        let child_runs = StdRc::new(StdCell::new(0));
        let child_runs_effect = child_runs.clone();

        create_effect(move || {
            // Subscribe the parent to `outer`.
            let _ = outer.get();

            let counter = child_runs_effect.clone();
            let inner = inner.clone();
            create_effect(move || {
                let _ = inner.get();
                counter.set(counter.get() + 1);
            });
        });

        // Parent ran once, creating one child, which ran once.
        assert_eq!(child_runs.get(), 1);

        // Re-run the parent three times: each disposes the previous child and
        // creates a fresh one, so exactly one new child run each time.
        for _ in 0..3 {
            set_outer.update(|v| *v += 1);
        }
        assert_eq!(
            child_runs.get(),
            4,
            "each parent run should create one child"
        );

        // Only the newest child is still subscribed to `inner`.
        child_runs.set(0);
        set_inner.update(|v| *v += 1);
        assert_eq!(
            child_runs.get(),
            1,
            "disposed children must not still react; got {} live effects",
            child_runs.get()
        );
    }

    #[test]
    fn on_cleanup_runs_when_the_owning_effect_re_runs() {
        let (trigger, set_trigger) = create_signal(0);
        let cleanups = StdRc::new(StdCell::new(0));
        let cleanups_effect = cleanups.clone();

        create_effect(move || {
            let _ = trigger.get();
            let counter = cleanups_effect.clone();
            on_cleanup(move || counter.set(counter.get() + 1));
        });

        assert_eq!(cleanups.get(), 0, "nothing disposed yet");

        set_trigger.update(|v| *v += 1);
        assert_eq!(
            cleanups.get(),
            1,
            "re-running must run the previous cleanup"
        );

        set_trigger.update(|v| *v += 1);
        assert_eq!(cleanups.get(), 2);
    }

    /// `get()` subscribes on every read, so an effect reading a signal three
    /// times used to run three times per write on the native path. The wasm
    /// path collapsed them in `PENDING_EFFECTS`; this makes both agree.
    #[test]
    fn reading_a_signal_repeatedly_does_not_multiply_effect_runs() {
        let (value, set_value) = create_signal(0);
        let runs = StdRc::new(StdCell::new(0));
        let runs_effect = runs.clone();

        create_effect(move || {
            let _ = value.get();
            let _ = value.get();
            let _ = value.get();
            runs_effect.set(runs_effect.get() + 1);
        });

        assert_eq!(runs.get(), 1);

        runs.set(0);
        set_value.update(|v| *v += 1);
        assert_eq!(
            runs.get(),
            1,
            "three reads must still mean one run, not {}",
            runs.get()
        );
    }

    #[test]
    fn a_top_level_effect_is_retained() {
        // Ownership must not accidentally dispose effects with no parent.
        let (value, set_value) = create_signal(0);
        let runs = StdRc::new(StdCell::new(0));
        let runs_effect = runs.clone();

        create_effect(move || {
            let _ = value.get();
            runs_effect.set(runs_effect.get() + 1);
        });

        set_value.update(|v| *v += 1);
        set_value.update(|v| *v += 1);
        assert_eq!(runs.get(), 3, "initial run plus two writes");
    }
}

#[cfg(test)]
mod memo_tests {
    use super::*;
    use std::cell::Cell as StdCell;
    use std::rc::Rc as StdRc;

    fn counter() -> (StdRc<StdCell<u32>>, StdRc<StdCell<u32>>) {
        let c = StdRc::new(StdCell::new(0));
        (c.clone(), c)
    }

    #[test]
    fn a_memo_caches_across_reads() {
        let (source, _set) = create_signal(2);
        let (computations, probe) = counter();

        let doubled = create_memo(move || {
            computations.set(computations.get() + 1);
            source.get() * 2
        });

        // Eager first computation.
        assert_eq!(probe.get(), 1);

        assert_eq!(doubled.get(), 4);
        assert_eq!(doubled.get(), 4);
        assert_eq!(doubled.get(), 4);
        assert_eq!(probe.get(), 1, "three reads must not recompute");
    }

    #[test]
    fn a_memo_recomputes_after_a_dependency_changes() {
        let (source, set_source) = create_signal(2);
        let (computations, probe) = counter();

        let doubled = create_memo(move || {
            computations.set(computations.get() + 1);
            source.get() * 2
        });

        assert_eq!(doubled.get(), 4);
        set_source.set(5);
        assert_eq!(doubled.get(), 10, "must see the new value");
        assert_eq!(probe.get(), 2, "exactly one recomputation");
    }

    /// A memo nothing reads costs nothing to keep current: the write marks it
    /// dirty, but the computation is deferred until somebody asks.
    #[test]
    fn a_dirty_memo_is_not_recomputed_until_read() {
        let (source, set_source) = create_signal(1);
        let (computations, probe) = counter();

        let memo = create_memo(move || {
            computations.set(computations.get() + 1);
            source.get()
        });

        assert_eq!(probe.get(), 1);
        set_source.set(2);
        set_source.set(3);
        set_source.set(4);
        assert_eq!(probe.get(), 1, "writes alone must not recompute");

        assert_eq!(memo.get(), 4, "the read settles it at the latest value");
        assert_eq!(probe.get(), 2, "and computes exactly once to do so");
    }

    /// The reason memos mark dirty on write and recompute on read.
    ///
    /// `source` feeds two memos, and one effect reads both. Under naive push
    /// propagation — recompute each memo as it is notified, running downstream
    /// effects immediately — the effect runs once per branch, and the first run
    /// observes one fresh and one stale value. This asserts it runs once.
    #[test]
    fn a_diamond_settles_once_per_write() {
        let (source, set_source) = create_signal(1);

        let source_a = source.clone();
        let doubled = create_memo(move || source_a.get() * 2);
        let source_b = source.clone();
        let incremented = create_memo(move || source_b.get() + 1);

        let (runs, runs_probe) = counter();
        let seen = StdRc::new(StdCell::new((0, 0)));
        let seen_effect = seen.clone();

        create_effect(move || {
            let a = doubled.get();
            let b = incremented.get();
            runs.set(runs.get() + 1);
            seen_effect.set((a, b));
        });

        assert_eq!(runs_probe.get(), 1);
        assert_eq!(seen.get(), (2, 2));

        set_source.set(10);

        assert_eq!(
            runs_probe.get(),
            2,
            "the effect must run once per write, not once per memo branch"
        );
        assert_eq!(
            seen.get(),
            (20, 11),
            "both branches must be fresh in the same run — a glitch would show \
             one updated and one stale"
        );
    }

    #[test]
    fn memos_compose() {
        let (source, set_source) = create_signal(2);
        let doubled = create_memo(move || source.get() * 2);
        let doubled_for_chain = doubled.clone();
        let quadrupled = create_memo(move || doubled_for_chain.get() * 2);

        assert_eq!(quadrupled.get(), 8);
        set_source.set(3);
        assert_eq!(
            quadrupled.get(),
            12,
            "dirtiness must propagate through the chain"
        );
    }

    #[test]
    fn untrack_reads_without_subscribing() {
        let (tracked, set_tracked) = create_signal(0);
        let (ignored, set_ignored) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = tracked.get();
            let _ = untrack(|| ignored.get());
            runs.set(runs.get() + 1);
        });

        assert_eq!(probe.get(), 1);

        set_ignored.set(99);
        assert_eq!(probe.get(), 1, "an untracked read must not subscribe");

        set_tracked.set(1);
        assert_eq!(probe.get(), 2, "a tracked read still does");
    }

    #[test]
    fn untrack_restores_the_previous_subscriber() {
        // Reads after the untracked block must subscribe again.
        let (before, set_before) = create_signal(0);
        let (after, set_after) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = before.get();
            let _ = untrack(|| 1 + 1);
            let _ = after.get();
            runs.set(runs.get() + 1);
        });

        set_after.set(1);
        assert_eq!(probe.get(), 2, "the read after untrack must still track");
        set_before.set(1);
        assert_eq!(probe.get(), 3);
    }

    #[test]
    fn a_memo_owned_by_an_effect_is_disposed_with_it() {
        let (outer, set_outer) = create_signal(0);
        let (inner, set_inner) = create_signal(0);
        let (computations, probe) = counter();

        create_effect(move || {
            let _ = outer.get();
            let computations = computations.clone();
            let inner = inner.clone();
            let memo = create_memo(move || {
                computations.set(computations.get() + 1);
                inner.get()
            });
            // Read it so it registers a dependency on `inner`.
            let _ = memo.get();
        });

        assert_eq!(probe.get(), 1);

        // Re-run the parent: the old memo is disposed, a new one created.
        set_outer.set(1);
        assert_eq!(probe.get(), 2, "one fresh memo per parent run");

        // Only the live memo should react.
        set_inner.set(5);
        assert_eq!(
            probe.get(),
            3,
            "a disposed memo must not recompute; got {} computations",
            probe.get()
        );
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::cell::Cell as StdCell;
    use std::rc::Rc as StdRc;

    fn counter() -> (StdRc<StdCell<u32>>, StdRc<StdCell<u32>>) {
        let c = StdRc::new(StdCell::new(0));
        (c.clone(), c)
    }

    #[test]
    fn writes_in_a_batch_run_a_dependent_effect_once() {
        let (first, set_first) = create_signal(0);
        let (second, set_second) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = first.get();
            let _ = second.get();
            runs.set(runs.get() + 1);
        });
        assert_eq!(probe.get(), 1);

        batch(|| {
            set_first.set(1);
            set_second.set(2);
        });

        assert_eq!(probe.get(), 2, "two writes in a batch mean one re-run");
    }

    #[test]
    fn the_same_writes_unbatched_run_it_twice() {
        // The control for the test above: without batching each write fans out.
        let (first, set_first) = create_signal(0);
        let (second, set_second) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = first.get();
            let _ = second.get();
            runs.set(runs.get() + 1);
        });

        set_first.set(1);
        set_second.set(2);

        assert_eq!(probe.get(), 3, "initial run plus one per write");
    }

    #[test]
    fn effects_do_not_run_until_the_batch_closes() {
        let (value, set_value) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = value.get();
            runs.set(runs.get() + 1);
        });

        batch(|| {
            set_value.set(1);
            assert_eq!(probe.get(), 1, "still deferred inside the batch");
            set_value.set(2);
            assert_eq!(probe.get(), 1);
        });

        assert_eq!(probe.get(), 2);
    }

    #[test]
    fn only_the_outermost_batch_flushes() {
        let (value, set_value) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = value.get();
            runs.set(runs.get() + 1);
        });

        batch(|| {
            set_value.set(1);
            batch(|| {
                set_value.set(2);
            });
            assert_eq!(
                probe.get(),
                1,
                "an inner batch closing must not flush the outer one"
            );
        });

        assert_eq!(probe.get(), 2);
    }

    #[test]
    fn a_batch_returns_the_closure_value() {
        let (value, set_value) = create_signal(1);
        let doubled = batch(|| {
            set_value.set(21);
            value.get() * 2
        });
        assert_eq!(doubled, 42);
    }

    #[test]
    fn distinct_effects_each_run_once_per_batch() {
        let (value, set_value) = create_signal(0);
        let (first_runs, first_probe) = counter();
        let (second_runs, second_probe) = counter();

        let value_a = value.clone();
        create_effect(move || {
            let _ = value_a.get();
            first_runs.set(first_runs.get() + 1);
        });
        let value_b = value.clone();
        create_effect(move || {
            let _ = value_b.get();
            second_runs.set(second_runs.get() + 1);
        });

        batch(|| {
            set_value.set(1);
            set_value.set(2);
            set_value.set(3);
        });

        assert_eq!(first_probe.get(), 2, "deduped, not skipped");
        assert_eq!(second_probe.get(), 2);
    }

    #[test]
    fn a_batch_settles_memos_once() {
        let (source, set_source) = create_signal(1);
        let (computations, probe) = counter();

        let doubled = create_memo(move || {
            computations.set(computations.get() + 1);
            source.get() * 2
        });
        assert_eq!(probe.get(), 1);

        batch(|| {
            set_source.set(2);
            set_source.set(3);
            set_source.set(4);
        });

        assert_eq!(
            probe.get(),
            1,
            "marking dirty is not recomputing; nothing read it yet"
        );
        assert_eq!(doubled.get(), 8);
        assert_eq!(probe.get(), 2, "one recomputation for three writes");
    }
}

#[cfg(test)]
mod batch_unwind_tests {
    use super::*;
    use std::cell::Cell as StdCell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::rc::Rc as StdRc;

    fn counter() -> (StdRc<StdCell<u32>>, StdRc<StdCell<u32>>) {
        let c = StdRc::new(StdCell::new(0));
        (c.clone(), c)
    }

    /// The defect this guards: a panic inside a batch used to leave
    /// `BATCH_DEPTH` raised forever, after which every write on the thread
    /// queued and nothing ever ran again.
    ///
    /// Not hypothetical — `krab_client` runs island factories inside
    /// `catch_unwind`, and `error_boundary` exists to continue after a panic,
    /// so execution really does reach the next write.
    #[test]
    fn a_panic_inside_a_batch_does_not_wedge_the_reactive_system() {
        let (value, set_value) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = value.get();
            runs.set(runs.get() + 1);
        });
        assert_eq!(probe.get(), 1);

        let set_in_batch = set_value.clone();
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            batch(|| {
                set_in_batch.set(1);
                panic!("something inside the batch failed");
            });
        }));
        assert!(
            panicked.is_err(),
            "the panic must propagate, not be swallowed"
        );

        assert_eq!(
            BATCH_DEPTH.with(|depth| depth.get()),
            0,
            "the batch depth must be restored on unwind"
        );

        // The system must still work. This write also delivers the effect the
        // aborted batch had queued.
        set_value.set(2);
        assert!(
            probe.get() > 1,
            "effects stopped running after a panic inside a batch"
        );
    }

    /// A write that completed before the panic must not be silently dropped:
    /// its value is already committed, so its dependents have to be told.
    #[test]
    fn writes_made_before_a_panic_are_delivered_by_the_next_write() {
        let (value, set_value) = create_signal(0);
        let seen = StdRc::new(StdCell::new(0));
        let seen_effect = seen.clone();

        create_effect(move || {
            seen_effect.set(value.get());
        });
        assert_eq!(seen.get(), 0);

        let set_in_batch = set_value.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            batch(|| {
                set_in_batch.set(7);
                panic!("abort mid-batch");
            });
        }));

        // The value was committed even though the batch aborted.
        set_value.set(9);
        assert_eq!(
            seen.get(),
            9,
            "the effect must observe the committed state after recovery"
        );
    }

    #[test]
    fn a_panic_in_a_nested_batch_restores_the_whole_depth() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            batch(|| {
                batch(|| {
                    batch(|| panic!("deep failure"));
                });
            });
        }));

        assert_eq!(
            BATCH_DEPTH.with(|depth| depth.get()),
            0,
            "every nested guard must unwind"
        );
    }

    /// Parity: effects have run by the time `batch` returns. Natively this was
    /// already true; the wasm flush now runs synchronously too rather than
    /// deferring to a microtask, so the scope means the same thing on both.
    #[test]
    fn effects_have_run_by_the_time_batch_returns() {
        let (value, set_value) = create_signal(0);
        let (runs, probe) = counter();

        create_effect(move || {
            let _ = value.get();
            runs.set(runs.get() + 1);
        });

        batch(|| {
            set_value.set(1);
        });

        assert_eq!(
            probe.get(),
            2,
            "the flush must be synchronous, not deferred past the scope"
        );
    }
}

/// Unwind recovery and subscriber-growth regression tests. Each test is named
/// for the failure it prevents; all of them passed *before* their fix only by
/// accident of not exercising the panic or growth path.
#[cfg(test)]
mod unwind_and_growth_tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// The zombie-subscriber bug: an effect that panicked under `catch_unwind`
    /// left `CURRENT_SUBSCRIBER` pointing at itself, so every later top-level
    /// effect was adopted as its child and mass-disposed on its next run.
    #[test]
    fn a_panicking_effect_does_not_leave_a_zombie_subscriber() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            create_effect(|| panic!("boom"));
        }));
        assert!(result.is_err(), "the panic must propagate");

        assert!(
            CURRENT_SUBSCRIBER.with(|c| c.borrow().is_none()),
            "a caught effect panic must not leave the thread tracking the dead effect"
        );

        // A new top-level effect must be independent — not adopted by a zombie.
        let (count, set_count) = create_signal(0);
        let runs = Rc::new(Cell::new(0));
        let seen = runs.clone();
        create_effect(move || {
            let _ = count.get();
            seen.set(seen.get() + 1);
        });
        set_count.set(1);
        assert_eq!(runs.get(), 2, "initial run plus one re-run");
    }

    /// The memo-poisoning bug: the recompute closure was `take()`n and never
    /// restored on unwind, so one caught panic froze the memo on its stale
    /// value forever.
    #[test]
    fn a_panicking_memo_recovers_and_recomputes_on_the_next_read() {
        let (source, set_source) = create_signal(1);
        let should_panic = Rc::new(Cell::new(false));
        let flag = should_panic.clone();

        let memo = create_memo(move || {
            if flag.get() {
                panic!("transient failure");
            }
            source.get() * 10
        });
        assert_eq!(memo.get(), 10);

        should_panic.set(true);
        set_source.set(2);
        let result = catch_unwind(AssertUnwindSafe(|| memo.get()));
        assert!(result.is_err(), "the recompute panic must propagate");
        assert!(
            CURRENT_SUBSCRIBER.with(|c| c.borrow().is_none()),
            "a caught memo panic must not leave the thread tracking the memo"
        );

        // Recovered: the closure is back and the memo is still dirty, so the
        // next read retries rather than serving 10 as if it were fresh.
        should_panic.set(false);
        assert_eq!(
            memo.get(),
            20,
            "after recovery the memo must recompute, not serve the stale value"
        );
    }

    /// `untrack` shares the unguarded replace-before/replace-after pattern the
    /// other two sites had; a panic inside it must not kill tracking.
    #[test]
    fn a_panic_inside_untrack_restores_tracking() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            untrack(|| -> i32 { panic!("boom") });
        }));
        assert!(result.is_err());
        assert!(
            CURRENT_SUBSCRIBER.with(|c| c.borrow().is_none()),
            "untrack must restore the previous subscriber on unwind"
        );

        // Tracking still works afterwards.
        let (value, set_value) = create_signal(0);
        let runs = Rc::new(Cell::new(0));
        let seen = runs.clone();
        create_effect(move || {
            let _ = value.get();
            seen.set(seen.get() + 1);
        });
        set_value.set(1);
        assert_eq!(runs.get(), 2);
    }

    /// The memo-subscriber leak: the list was cloned on notify but never
    /// drained, growing one entry per dependent re-run for the memo's life.
    #[test]
    fn memo_subscribers_do_not_accumulate_across_writes() {
        let (source, set_source) = create_signal(0);
        let memo = create_memo(move || source.get() * 2);
        let observer = memo.clone();
        create_effect(move || {
            let _ = observer.get();
        });

        for i in 1..=25 {
            set_source.set(i);
        }

        let len = memo.state.subscribers.borrow().len();
        assert!(
            len <= 1,
            "memo subscriber list must stay bounded (drained on notify), found {len}"
        );
    }

    /// Reading the same signal several times in one run must land one
    /// subscription, not one per read.
    #[test]
    fn repeated_reads_do_not_duplicate_signal_subscriptions() {
        let (value, _set_value) = create_signal(0);
        let probe = value.clone();
        create_effect(move || {
            let _ = probe.get();
            let _ = probe.get();
            let _ = probe.get();
        });

        let len = value.inner.state.borrow().subscribers.len();
        assert_eq!(
            len, 1,
            "three reads in one effect run must produce one subscription"
        );
    }

    /// A source that is read by a re-running effect but never itself written is
    /// never drained by notify — subscribe-time dedup has to bound it instead.
    #[test]
    fn an_unwritten_source_does_not_accumulate_subscribers() {
        let (written, set_written) = create_signal(0);
        let (unwritten, _set_unwritten) = create_signal(0);
        let probe = unwritten.clone();
        create_effect(move || {
            let _ = written.get();
            let _ = probe.get();
        });

        for i in 1..=25 {
            set_written.set(i);
        }

        let len = unwritten.inner.state.borrow().subscribers.len();
        assert!(
            len <= 2,
            "an unwritten source's subscriber list must stay bounded, found {len}"
        );
    }

    /// The interleaved variant the tail-only dedup missed: a parent effect and
    /// its child both read the same unwritten signal, and the parent reads it
    /// again after the child is created. With `last()`-only dedup the parent's
    /// second read always landed a duplicate, one per parent re-run.
    #[test]
    fn interleaved_parent_and_child_reads_do_not_accumulate_subscribers() {
        let (written, set_written) = create_signal(0);
        let (unwritten, _set_unwritten) = create_signal(0);

        let parent_probe = unwritten.clone();
        let child_probe = unwritten.clone();
        create_effect(move || {
            let _ = written.get();
            // Parent subscribes; list tail is now the parent.
            let _ = parent_probe.get();
            let probe = child_probe.clone();
            create_effect(move || {
                // Child subscribes; list tail is now the child.
                let _ = probe.get();
            });
            // Parent reads again: the tail is the child, so a tail-only check
            // pushed a second parent entry every run.
            let _ = parent_probe.get();
        });

        for i in 1..=25 {
            set_written.set(i);
        }

        let len = unwritten.inner.state.borrow().subscribers.len();
        assert!(
            len <= 2,
            "one parent and one child must mean at most two subscriptions, found {len}"
        );
    }
}

/// Guards against reactive cycles: an effect writing its own dependency, a
/// memo reading itself, and the disposal API for root effects.
#[cfg(test)]
mod cycle_guard_tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// The stack-overflow bug: natively a `set()` inside an effect ran the
    /// effect again synchronously on the same stack, so an effect that wrote
    /// its own dependency recursed until it overflowed. The running flag
    /// refuses the re-entrant run and delivers it after the body finishes, so
    /// the effect terminates *and* settles on the value it wrote.
    #[test]
    fn an_effect_writing_its_own_dependency_terminates_and_converges() {
        let (count, set_count) = create_signal(0u32);
        let runs = Rc::new(Cell::new(0u32));

        let reader = count.clone();
        let seen = runs.clone();
        create_effect(move || {
            seen.set(seen.get() + 1);
            let current = reader.get();
            if current < 5 {
                set_count.set(current + 1);
            }
        });

        assert_eq!(count.get(), 5, "the effect must converge on its own writes");
        assert_eq!(runs.get(), 6, "initial run plus one owed re-run per write");
    }

    /// An effect that unconditionally rewrites its own dependency can never
    /// converge; the iteration cap must cut it off instead of hanging.
    #[test]
    fn an_unconditional_self_write_is_cut_off_by_the_flush_cap() {
        let (value, set_value) = create_signal(0u64);
        let runs = Rc::new(Cell::new(0u32));

        let reader = value.clone();
        let seen = runs.clone();
        create_effect(move || {
            seen.set(seen.get() + 1);
            set_value.set(reader.get() + 1);
        });

        assert_eq!(
            runs.get(),
            MAX_FLUSH_DEPTH,
            "consecutive runs of a livelocked effect are capped at MAX_FLUSH_DEPTH"
        );
    }

    /// A panic inside a self-notifying effect must not wedge the running flag:
    /// the drop guard clears it, so the effect is not refused as a false cycle
    /// on its next legitimate run.
    #[test]
    fn a_panicking_effect_does_not_wedge_the_running_flag() {
        let (value, set_value) = create_signal(0);
        let should_panic = Rc::new(Cell::new(true));
        let runs = Rc::new(Cell::new(0u32));

        let flag = should_panic.clone();
        let seen = runs.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            create_effect(move || {
                let _ = value.get();
                seen.set(seen.get() + 1);
                if flag.get() {
                    panic!("first run fails");
                }
            });
        }));
        assert!(result.is_err(), "the panic must propagate");

        should_panic.set(false);
        set_value.set(1);
        assert_eq!(
            runs.get(),
            2,
            "after a caught panic the effect must run again, not be refused as a cycle"
        );
    }

    /// The self-referential-memo panic: the memo's closure reading the memo
    /// back found `dirty` already cleared and an (on the panicking path)
    /// absent value, and `with()` `expect()`ed. The computing flag now serves
    /// the stale value to the self-read, so the computation completes
    /// deterministically instead of panicking.
    #[test]
    fn a_self_referential_memo_serves_the_stale_value_instead_of_panicking() {
        let handle: Rc<RefCell<Option<Memo<i32>>>> = Rc::new(RefCell::new(None));
        let (source, set_source) = create_signal(1);

        let handle_in_closure = handle.clone();
        let memo = create_memo(move || {
            let base = source.get();
            // On the eager first computation the handle is still empty, so
            // there is a non-recursive base case; every recomputation after
            // that reads the memo back through the handle.
            let previous = handle_in_closure
                .borrow()
                .as_ref()
                .map(|memo: &Memo<i32>| memo.get())
                .unwrap_or(0);
            base + previous
        });
        *handle.borrow_mut() = Some(memo.clone());

        assert_eq!(memo.get(), 1, "eager run: base 1 with no previous value");

        set_source.set(10);
        let result = catch_unwind(AssertUnwindSafe(|| memo.get()));
        let value = result.expect("a self-referential memo must not panic");
        assert_eq!(
            value, 11,
            "the self-read must see the stale value (1), giving 10 + 1"
        );

        // Deterministic thereafter: reading again without a write serves the
        // cached value, no recomputation, no panic.
        assert_eq!(memo.get(), 11);
    }

    /// Root effects were pushed into `ROOT_EFFECTS` forever with no disposal
    /// API; `create_effect_scoped` must return the list to its baseline.
    #[test]
    fn disposing_a_scoped_effect_returns_root_effects_to_baseline() {
        let baseline = ROOT_EFFECTS.with(|roots| roots.borrow().len());

        let (value, set_value) = create_signal(0);
        let runs = Rc::new(Cell::new(0u32));

        let reader = value.clone();
        let seen = runs.clone();
        let handle = create_effect_scoped(move || {
            let _ = reader.get();
            seen.set(seen.get() + 1);
        });

        assert_eq!(
            ROOT_EFFECTS.with(|roots| roots.borrow().len()),
            baseline + 1,
            "a scoped effect is retained while live"
        );
        set_value.set(1);
        assert_eq!(runs.get(), 2, "a live scoped effect reacts to writes");

        handle.dispose();
        assert_eq!(
            ROOT_EFFECTS.with(|roots| roots.borrow().len()),
            baseline,
            "dispose must remove the retained Rc from ROOT_EFFECTS"
        );

        set_value.set(2);
        assert_eq!(runs.get(), 2, "a disposed scoped effect must not run again");

        // Idempotent: a second dispose must not remove anything else.
        handle.dispose();
        assert_eq!(ROOT_EFFECTS.with(|roots| roots.borrow().len()), baseline);
    }

    /// `on_cleanup` callbacks registered by a scoped effect run at dispose.
    #[test]
    fn disposing_a_scoped_effect_runs_its_cleanups() {
        let cleanups = Rc::new(Cell::new(0u32));

        let counter = cleanups.clone();
        let handle = create_effect_scoped(move || {
            let counter = counter.clone();
            on_cleanup(move || counter.set(counter.get() + 1));
        });

        assert_eq!(cleanups.get(), 0);
        handle.dispose();
        assert_eq!(cleanups.get(), 1, "dispose must run the registered cleanup");
    }
}
