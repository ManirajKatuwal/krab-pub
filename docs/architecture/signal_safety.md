# Signal System — Threading Constraints and Cycle Guards

## Overview

Krab's reactive signal system (`krab_core::signal`) provides fine-grained
reactivity for UI components via `ReadSignal<T>` / `WriteSignal<T>` /
`create_effect`.

## Threading model

| Property | Value |
|----------|-------|
| Thread-safe? | **No** |
| `Send` | `!Send` (compile-time) |
| `Sync` | `!Sync` (compile-time) |
| Underlying types | `Rc<RefCell<...>>` |

The implementation deliberately uses `Rc` and `RefCell` to avoid any locking
overhead.  This means:

1. **You cannot move a signal to another thread.**  Attempting to do so is a
   **compile error** — `Rc` is `!Send` and `!Sync`.

2. **Every signal graph must be created and destroyed on the same thread.**
   In server-side rendering (SSR) mode, create the signal graph inside the
   request handler; it lives only for the lifetime of that handler invocation.
   In WASM mode the single JavaScript event loop thread is the only thread, so
   this is automatically satisfied.

3. **Do not wrap signals in `Arc<Mutex<...>>`.**  If you need truly shared
   state across async tasks or threads, use tokio primitives:
   - `Arc<tokio::sync::RwLock<T>>` for shared readers with occasional writes
   - `tokio::sync::watch` for broadcast state
   - `tokio::sync::mpsc` for producer/consumer queues

## Why this design?

- **Zero synchronisation overhead** for the common case (single-threaded UI).
- **Rust type system enforces the constraint** — no runtime panics for
  threading violations; it is caught at compile time.
- Matches the mental model of WASM/browser environments where there is exactly
  one thread.

## Server-side rendering pattern

```rust
// Each request spawns a future on the Tokio thread pool.
// Create signals inside the future — they stay on the same OS thread
// for the lifetime of the async block.
async fn render_page() -> String {
    let (count, set_count) = krab_core::signal::create_signal(0);
    set_count.set(42);
    // render to HTML string — signals are dropped here
    format!("<p>{}</p>", count.get())
}
```

## Cycle guards and re-entrancy

The graph is single-threaded, so the dangerous cycles are *stack* cycles: a
reactive node re-entering itself synchronously. Three guards bound them. All
three follow the same policy — **no panic from the guard itself, an error
event on the structured log, and a deterministic value** — because a reactive
cycle is an application bug the framework must survive, not crash on.

### Effect self-writes (`signal_effect_cycle_detected`)

Natively, a `set()` is delivered synchronously, so an effect that writes one of
its **own** dependencies used to re-enter `run_effect` on the same stack and
recurse until it overflowed. Each effect now carries a `running` flag (cleared
by a drop guard, so a panic caught upstream cannot wedge it):

- A notification that arrives for an effect **while its body is on the stack**
  is refused, and `signal_effect_cycle_detected` is emitted at `ERROR` level —
  once per effect, since it identifies a coding bug, not a per-occurrence
  condition.
- The refused run is *owed*, not dropped: after the body finishes, the effect
  runs again, so it still converges on the value it wrote. An effect that
  increments its own counter toward a bound settles exactly at the bound.
- Owed re-runs are capped at 64 consecutive iterations. An effect that
  unconditionally rewrites its own dependency can never converge; the cap cuts
  it off with `signal_flush_depth_exceeded` instead of livelocking the thread.

On wasm the unbatched path coalesces into a microtask, which does not recurse
on the same stack; the shared `run_effect` guard still protects the synchronous
paths that exist there too (a `batch` flush inside an effect body).

### Nested synchronous flushes (`signal_flush_depth_exceeded`)

The running flag is per-effect, so it cannot bound a *chain* of distinct
effects each writing another's dependency. The native synchronous flush
therefore tracks its nesting depth in a thread-local, also capped at 64. At the
cap the delivery is dropped with `signal_flush_depth_exceeded`; the written
values are already committed, so the next legitimate write re-runs the
dependents.

### Self-referential memos (`memo_self_reference_detected`)

A memo whose computation reads the memo back used to panic (`Memo::with`
`expect()`ed on the missing value the first time the cycle bit). Each memo now
carries a `computing` flag (cleared by the same drop guard that restores its
other state on unwind). A read of a memo **while its own computation is on the
stack**:

- emits `memo_self_reference_detected` at `ERROR` level,
- is treated as **untracked** — the memo does not subscribe to itself, and
- is served the **stale** cached value, so the enclosing computation completes
  deterministically: `new = f(stale)`.

`create_memo` computes eagerly before returning the handle, so a value always
exists by the time a self-read is reachable through the public API (a handle
cannot be captured by the closure before it exists). The one theoretical
corner — a self-read during a *first* computation that has no previous value —
is unsatisfiable (there is no `T` to serve) and is kept as a guarded invariant:
error event, then a panic with an explicit message, which the memo's unwind
guards make recoverable and retryable like any other caught computation panic.

### Effect disposal (`create_effect_scoped`)

`create_effect` retains top-level effects for the lifetime of the thread —
permanent by design, matching hydration effects that live as long as the page.
For effects scoped to something shorter-lived, `create_effect_scoped` returns
an `EffectHandle`; `dispose()` marks the effect disposed, runs its cleanups,
disposes everything it owns, and removes the retained `Rc` from the root-effect
list by pointer identity, so the memory is actually reclaimable. Scoped effects
are never adopted by an enclosing effect — the handle is their sole owner.
`dispose()` is idempotent.

### Subscriber lists stay bounded

Subscribing scans the whole (short) subscriber list, not just its tail, so
interleaved reads — parent effect, child effect, parent again on the same
never-written signal — deduplicate instead of accumulating one entry per run.
Dead `Weak` entries are swept out during the same scan.

## Compile-time guard

The authoritative enforcement comes from the Rust compiler via the underlying
`Rc<RefCell<...>>` representation in `krab_core::signal`.

The `signals_are_not_send_sync` test in `krab_core/src/signal.rs` is a
documentation/sanity test, not a strict compile-fail gate by itself.

In practice, making `ReadSignal<T>`/`WriteSignal<T>` `Send` or `Sync` would
require a deliberate structural refactor (for example replacing `Rc` with
thread-safe primitives), not an accidental change.
