# Signal System — Threading Constraints

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

## Compile-time guard

The authoritative enforcement comes from the Rust compiler via the underlying
`Rc<RefCell<...>>` representation in `krab_core::signal`.

The `signals_are_not_send_sync` test in `krab_core/src/signal.rs` is a
documentation/sanity test, not a strict compile-fail gate by itself.

In practice, making `ReadSignal<T>`/`WriteSignal<T>` `Send` or `Sync` would
require a deliberate structural refactor (for example replacing `Rc` with
thread-safe primitives), not an accidental change.
