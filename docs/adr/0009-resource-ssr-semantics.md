# ADR 0009: `Resource` Renders Synchronously on the Server — Initial Value or Pending, Never Blocking

## Status

**Accepted** — 2026-08-11. Implemented in the same change (Phase 6.2 of the
reactive-core plan).

Builds on [ADR 0001](0001-hydration-markers.md) (hydration markers) and the
`Action` precedent (`krab_core::action`): APIs used inside `#[island]` bodies
exist on both compilation targets, with the server side inert.

## Context

`Action` covers the write side of async: dispatch an operation, observe
`pending` / `value` / `error`. The read side — "this component needs data that
comes from a future" — has no primitive. Users hand-roll it with `create_signal`
plus `Action`-style dispatch on mount, which means every data load reinvents
loading state, error state, and refetch.

A `Resource` must answer one question before its API can exist: **what happens
during server-side rendering, where the future cannot be awaited mid-render?**
The three candidate semantics:

1. **Block** — await the future server-side; render with the resolved data.
2. **Hydrate-empty** — render the pending state; the client fetches after
   hydration.
3. **Stream** — render a fallback slot, flush it early, stream the resolved
   HTML later and swap it in.

### What the codebase actually permits

- **All `Node` building is synchronous.** No component, island, or page in the
  workspace is an `async fn` returning `Node`. This is not an accident:
  `krab_core::Node` is `!Send` (it holds `Rc` via `Dynamic` and
  `EventListener`), so a `Node` cannot be held across an `.await` in the Tokio
  handlers that drive SSR. "Block the render" therefore does not mean inserting
  an `.await` — it means making the entire component tree async, an
  architectural rewrite of the render pipeline, the `view!` expansion, and
  every call site.

- **Async-then-sync is already the working pattern.** Route handlers *are*
  async. `service_frontend` fetches data in the handler, then builds nodes
  synchronously and passes data down as props. Server-fetched data reaching a
  component is a solved problem; it just has no reactive wrapper on the client
  side.

- **Island props are a proven serialization channel.** `#[island]` serializes
  props to JSON in `data-props`; the client decodes them during hydration
  (states `ok` / `patched` / `error` / `decode-error` / `missing-definition`).
  Any value that can be a prop can reach the browser without new protocol.

- **Streaming has no client half.** `render_stream.rs` provides
  `ChunkedStreamWriter` and `<!--krab:suspense:{id}:{state}-->` markers;
  `service_frontend` writes them and its cache layer parses them back out.
  Nothing in `krab_client` consumes them — there is no swap-in logic, no
  template slots, no out-of-order delivery. Choosing "stream" means building
  that half first and resolving how a half-resolved stream interacts with the
  ISR cache, which stores finished HTML per path.

- **ISR wants deterministic output.** `IsrCache` stores the rendered HTML
  string. A synchronous render is deterministic given its inputs; a render
  whose output depends on which futures resolved before the cache snapshot is
  not.

## Decision

`create_resource` renders **synchronously** on the server, in one of two
states, and the fetch is **client-driven**:

```rust
// Client fetches after hydration; SSR renders the pending state.
let user = create_resource(move || user_id.get(), |id| async move { fetch_user(id).await });

// SSR renders with the value; the client hydrates with the same value and
// does NOT refetch on mount. `initial` typically arrives through island props,
// fetched by the async route handler.
let user = create_resource_with_initial(
    props.user,                       // Option<User>, serialized via data-props
    move || user_id.get(),
    |id| async move { fetch_user(id).await },
);
```

Semantics:

- `Resource` exposes `state() -> ReadSignal<ResourceState>` with
  `Pending` / `Ready` / `Error(String)`, and `value() -> ReadSignal<Option<T>>`
  as a separate signal, plus `refetch()`. The state carries no payload
  deliberately: keeping the value in its own signal is what lets a failed
  refetch move `state` to `Error` while `value` retains the last good data,
  and a refetch shows `Pending` without blanking what is on screen. A
  generation counter discards superseded responses, as in `Action`.
- **On the server**, the constructor never polls the future. With an initial
  value it renders `Ready`; without one it renders `Pending`. Construction and
  every read are pure signal operations — the same inertness contract
  `Action::dispatch` has, and testable natively the same way.
- **On the client**, hydration with an initial value starts in `Ready` and does
  not fetch; without one it starts in `Pending` and fetches immediately. The
  source closure is tracked: when its value changes, the resource refetches
  (through the generation counter, so a stale in-flight response cannot land).
- The blocking semantic is **rejected**, not deferred: it contradicts `!Send`
  nodes and synchronous tree building, and the async-handler-plus-props pattern
  already delivers everything blocking would, with the `initial` path giving it
  a reactive client-side continuation.
- The streaming semantic is **deferred, and the API is forward-compatible with
  it**: streaming changes *where the initial value comes from* (an inline
  payload flushed after the shell instead of `data-props`), not the `Resource`
  contract. A later ADR can add the client half of the suspense markers and
  feed the same initial-value path without breaking any `create_resource` call
  site.

No new wire protocol, no new hydration states, no ISR changes: a page using
resources renders deterministically, so cached entries stay byte-stable given
their inputs.

## Consequences

- **First paint is honest, not magical.** Data a page needs for SEO or first
  paint must be fetched in the async route handler and passed as
  props/`initial`. A resource without `initial` renders its pending state into
  the cached/streamed HTML. This is the islands trade: the page shell is the
  server's job, per-island data lifecycles are the client's.
- The two dead-surface findings become actionable: `LoadingState` /
  `LoadingFallback` (`loading.rs`, zero consumers) either back `Resource`'s
  pending rendering or are removed; the suspense-marker writer remains
  server-only until the streaming ADR, and must not be documented as if the
  client reacts to it.
- `Resource` lives in `krab_core` beside `Action`, for the same reason `Action`
  moved there: `#[island]` bodies compile for both targets, and a wasm-only API
  forces `#[cfg(target_arch)]` back into user code.
- Browser tests carry the state machine (fetch-on-hydrate, initial-no-refetch,
  source-change refetch, superseded-response discard); native tests pin the
  SSR inertness contract, exactly as `action::tests` does.

## Alternatives considered

**Block the render.** Rejected above; additionally couples time-to-first-byte
to the slowest data source with no fallback story, which the render-policy and
streaming-SLO work exists specifically to avoid.

**Stream now.** Requires building the entire client half (marker consumption,
slot swapping, ordering) plus an ISR answer for half-resolved pages, to ship a
primitive whose 90% case — "load data for this island, show a spinner, swap it
in" — hydrate-empty already covers. Deferred until the demand is demonstrated,
with the `initial` path reserved as its integration point.

**Refetch-on-hydrate even with `initial` (Leptos-style resource replay).**
Rejected: it doubles the load on every data source for pages that already
shipped the data in props, and the divergence it guards against (server data
staler than hydration time) is better handled by an explicit `refetch()` or a
short ISR revalidate window.
