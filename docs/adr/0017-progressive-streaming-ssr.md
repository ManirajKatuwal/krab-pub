# ADR 0017: Progressive (Out-of-Order) Streaming SSR on `<Suspense>`

## Status

**Accepted** — 2026-09-30. Implemented in the same change, for release in
`0.6.0`.

Supersedes the "streaming semantic is deferred" clause of
[ADR 0009](0009-resource-ssr-semantics.md); the rest of ADR 0009 — a
non-streaming render never waits on data — stands. Builds on
[ADR 0016](0016-suspense-boundaries.md) (`<Suspense>`).

## Context

`render_stream::ChunkedStreamWriter` split a *finished* render into chunks, and
`service_frontend` returned the page as one `String`. Time to first byte was
therefore the time to render everything, including any data the handler
awaited first. ADR 0009 deferred progressive streaming until three things
existed: a view-level boundary to stream (now `<Suspense>`), a client half that
consumes the markers, and an answer for caches holding half-resolved pages.

Constraints carried over:

- `Node` is `!Send` (it holds `Rc`), so a tree cannot cross an `.await` in a
  Tokio handler or move between threads.
- Rendering is synchronous; ADR 0009 rejected making it async.
- Krab's security headers send `Content-Security-Policy: script-src 'self'
  'wasm-unsafe-eval'`, fixed, with no nonce. **Inline scripts do not run.**

## Decision

```rust
// krab_core::render_stream — feature `rest`, not on wasm32
pub fn render_to_stream<F>(render: F) -> RenderStream
where F: FnOnce() -> Node + Send + 'static;
pub fn render_to_stream_with<F>(options: StreamOptions, render: F) -> RenderStream;
pub struct StreamOptions { pub timeout: Duration, pub swap_script_src: String, pub doctype: bool }
pub struct RenderStream;   // impl Stream<Item = Result<Bytes, Infallible>> + Send
pub const STREAM_SWAP_SCRIPT: &str;       // the browser half
pub const STREAM_SWAP_SCRIPT_PATH: &str;  // "/_krab/stream.js"

// krab_core::resource
impl<S, T: Clone + Send + 'static> Resource<S, T> {
    pub fn with_server_loader<L, Fut, E>(self, loader: L) -> Self
    where L: FnOnce() -> Fut, Fut: Future<Output = Result<T, E>> + Send + 'static, E: Display;
}
```

`axum::body::Body::from_stream(render_to_stream(|| page()))` serves it.

### One render thread per response

`render` is a `Send` closure that *builds* the tree; it runs on a dedicated
blocking thread (`spawn_blocking`, or a plain thread outside a runtime), inside
a fresh owner. Every `Node` of the response — the page, each boundary's
children, every re-render — is created, rendered and dropped on that thread.
Only strings leave it, over a bounded channel. This is the whole answer to
`!Send`: the tree never moves.

### Server loaders

A resource opts in with `with_server_loader`. The loader is a `FnOnce`
returning a `Send` future with `Send` output: it runs on the Tokio runtime,
off the render thread, so it can use the application's connection pools.
Its result is sent back to the render thread and applied to the resource's
signals there.

**The non-blocking guarantee of ADR 0009 is kept by construction:** outside a
streaming render (`stream_active()` is false), on `wasm32`, or when the
resource already has a value, the loader is never *called*, so no future is
even created. An ordinary render is exactly as before.

### Protocol

1. The page renders. A `<Suspense>` boundary that is pending, under an active
   stream, whose pending sources **all** have server loaders, is *deferred*:
   `<!--krab:suspense:{id}:pending-->fallback<!--/krab:suspense:{id}-->`. A
   boundary with any loader-less pending source cannot be resolved by the
   server, so it renders its fallback as final output (`… :resolved-->`), as
   without streaming.
2. The shell — everything before the last `</body>` — is flushed, plus
   `<script src="{swap_script_src}"></script>` when anything was deferred. The
   `</body></html>` tail is held back so streamed content lands inside the
   body.
3. Loader futures are spawned on the runtime. The render thread waits on their
   results with a deadline (`timeout`, default 10 s).
4. After each result is applied, every deferred boundary that is no longer
   pending is rendered on the render thread and flushed, in document order:

   ```html
   <template data-krab-suspense="{id}">…content…<!--krab:suspense:{id}:resolved--></template>
   <span data-krab-suspense-ready="{id}" hidden></span>
   ```

   Rendering resolved content can defer boundaries nested in it and register
   new loaders; the loop handles those before waiting again, so nesting
   streams naturally (the outer template always precedes the inner one).
5. When every deferred boundary has been emitted, or no loader is outstanding,
   or the deadline passes, leftovers get `<!--krab:suspense:{id}:error-->` —
   their fallback stays — and the tail closes the document.

### The browser half — an external script, because of CSP

The usual design emits an inline `<script>$swap("id")</script>` per boundary.
Under Krab's own `script-src 'self'` that never runs. So the swap runtime is an
**external, same-origin script** (`STREAM_SWAP_SCRIPT`, which the application
serves at `/_krab/stream.js`), loaded parser-blocking from the shell before any
template can arrive. It installs a `MutationObserver` on `document` and swaps a
boundary when its `data-krab-suspense-ready` sentinel is inserted — the sentinel
follows the `</template>`, so the template is complete even if the network
split it. The swap dispatches `krab:suspense-resolving` with
`detail: { id, nodes }` naming the fallback nodes, removes everything between
the boundary's two markers, replaces the end marker with the template's content
(which carries the `resolved` marker), removes the template and sentinel, and
dispatches `krab:suspense-resolved` with `detail: { id, nodes }` naming the
content.

`krab_client::hydrate()` installs document listeners for both: `unmount` on
each outgoing fallback element (an island hydrated in the fallback would
otherwise keep its closures and effects after its DOM is removed), and
`hydrate_within` on each swapped-in element. Islands in content that
arrived before the bundle loaded are already in the document when `hydrate()`
walks it.

A page served under a CSP that allows a nonce could inline the runtime; that is
not offered, because Krab's own CSP does not.

### Caching

The whole body always balances its markers: every `pending` is closed by a
`resolved` (in a template or rendered final) or an `error`. So
`is_finalized_ssr_snapshot` is `true` for a complete stream and `false` for any
prefix with an open deferral — a cache that stores only finalized snapshots
cannot store a half-streamed page. `service_frontend`'s middleware already
buffers a cacheable response (up to `KRAB_CACHE_MAX_BODY_BYTES`, streaming the
remainder through unstored) and checks finalization before an ISR write, so it
is correct unchanged; the cost is that an ISR miss is buffered and loses the
early flush. A streamed route that wants its TTFB should be uncached
(`CacheMode::None`, or no policy, like the `/streaming` demo).

## What is supported, and what is not

Supported:

- Page-level `<Suspense>` boundaries with server loaders, any number, nested,
  resolving in any order; boundaries without loaders render final fallbacks in
  the same response.
- Islands inside streamed content, hydrated on arrival; they get data through
  props, because the content is rendered after the loader finished.
- Client disconnect: the next write fails, the render thread stops and aborts
  outstanding loader tasks.
- A panicking page render: logged (`ssr_stream_render_panicked`); the stream
  ends. A panicking loader task never reports: its boundary stays on the
  fallback and is closed with `error` at the timeout.

Not supported (by design or deferred):

- **A deferred boundary inside an island.** The island hydrates against its own
  client resource, which starts `Pending` without the streamed data. Declare
  loaders on resources in page markup; an island needing streamed data takes it
  as props from a streamed page region.
- **Inline scripts in streamed content.** Whether a `<script>` parsed into a
  `<template>` runs when moved into the document is not something to rely on
  (and Krab's CSP blocks inline scripts anyway). Put behaviour in islands.
- **Mid-stream status or headers.** They are sent with the shell; a loader
  failure cannot turn the response into a 500. Render the error inside the
  boundary from the resource's `Error` state.
- **Byte budgets and `StreamTelemetry`** belong to `ChunkedStreamWriter` and do
  not apply to `render_to_stream`; it logs `ssr_stream_boundary_resolved`,
  `ssr_stream_boundaries_unresolved` and `ssr_stream_completed` with
  `duration_ms` instead.
- **Streaming without a Tokio runtime.** Called outside one, the page still
  streams its shell, but no loader runs and every boundary renders final.

## Consequences

- One blocking-pool thread is held per streaming response for its duration
  (bounded by `timeout`). Blocking-pool sizing now matters for streamed routes.
- `krab_core`'s `rest` feature gains a dependency on `futures-core` (already in
  the graph through axum and tokio) to implement `Stream`.
- Boundary ids are process-counter based (ADR 0016) and so unique within a
  response, which is all the swap needs.
- The home page keeps `ChunkedStreamWriter`: it has no async data, so there is
  nothing to defer.

## Alternatives considered

**Inline swap scripts with a nonce.** Standard, but requires a per-response
nonce in the CSP header, which Krab's fixed security headers do not send.

**Declarative shadow DOM / `<template shadowrootmode>` swaps.** No script, but
puts content in a shadow root, breaking page CSS and `querySelector`.

**Async render (`async fn` components).** Rejected by ADR 0009; it would make
`Node` `Send`-bound or pin every render to a `LocalSet`, rewriting the view
pipeline.

**Block the whole response on loaders (render once all resolve).** That is the
async-handler pattern ADR 0009 already supports; it gives up the early flush
that is the point of streaming.

## References

- `crates/framework/krab_core/src/render_stream/progressive.rs` — the render
  loop, the loader registry, `STREAM_SWAP_SCRIPT`, tests
- `crates/framework/krab_core/src/resource.rs` — `with_server_loader`
- `crates/framework/krab_core/src/suspense.rs` — deferral hook
- `crates/framework/krab_client/src/hydration.rs` — `install_suspense_listener`
- `services/service_frontend/src/streaming.rs` — `/streaming`, `/_krab/stream.js`;
  tests in `services/service_frontend/src/main.rs`
