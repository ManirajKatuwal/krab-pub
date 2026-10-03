# Hydration Invariants

## Building a bundle that hydrates

The runtime lives behind `krab_client`'s `web` feature — `hydrate()`, the
reconciler, the router's browser half, all of it. `web` is a **default** feature
as of 0.3.0. Before that it was opt-in, and a build that omitted it still
produced a valid, loadable module: `hydrate()` logged one line and returned. It
was ~15 KB against ~198 KB for the real runtime (~165 KB since 0.6.0 removed the
demo islands), and nothing on the page reported the difference.

```sh
wasm-pack build crates/framework/krab_client --release --target web -- --features web
```

Naming the feature is redundant now and is written out anyway, because the
failure it prevents is silent. If you need to check an artifact you did not
build, the runtime contains the string `data-krab-boundary-state` and a stub
does not:

```sh
grep -a "data-krab-boundary-state" dist/pkg/krab_client_bg.wasm
```

(Earlier docs and the `wasm-size` gate grepped for `Hydrating island`, a log
line compiled only under the `debug` feature, so the check failed on every
correct release build.)

`krab_client` ships no islands of its own since 0.6.0, so a bundle that
hydrates anything is always an **application** crate built for wasm32 — the
crate that defines the islands, depending on `krab_client` with `web`. See
`services/service_frontend_islands` and `examples/reference_apps/islands_rpc`.

The **server** side wants the opposite. `#[island]` selects its SSR half on
`not(feature = "web")` **of the crate that defines the island** — that half is
what emits the `data-island` / `data-props` wrapper the browser later finds. So
the islands crate declares its own `web` feature, the server links it without
that feature, and the wasm build turns it on; with `web` on, the same call
renders bare component markup and there is nothing to hydrate.

## Boundary contract

Krab hydration is boundary-scoped. Each `#[island]` instance owns a wrapper and a marker namespace:

- `data-island="<island-name>"`
- `data-props="<serialized-props-json>"`
- `data-krab-boundary="<island-name>"`
- `data-krab-boundary-id="<island-name>:<instance-number>"`
- `data-krab-boundary-state="ssr|hydrating|ok|patched|decode-error|error|missing-definition"`
- `data-krab-boundary-mismatches="<count>"`

## Element marker format

Hydratable elements inside an island receive:

```text
data-krab-node-id="<boundary-id>/<path>"
```

Rules:

- The root hydratable element path is `0`.
- Child paths append `.<index>`, for example `HomeCounter:3/0.1.0`.
- Marker namespaces do not cross island boundaries.
- When the server tree contains a nested island wrapper (`data-island`), outer marker annotation stops at that wrapper so the nested boundary can own its own marker space.

## Reconciliation rules

During `krab_client::hydrate()`:

1. The runtime locates each island wrapper by `data-island`.
2. It rebuilds the expected node tree from the island factory and re-applies the same marker format using `data-krab-boundary-id`.
3. For element nodes with `data-krab-node-id`, the hydrator prefers marker matches over positional tag matching.
4. If a matching marker exists later among siblings, the runtime moves that DOM node into place instead of replacing it.
5. Positional tag/text fallback is only used when marker data is unavailable, such as legacy DOM without node IDs.
6. Extra DOM nodes left over after reconciliation are removed.
7. If props fail to decode, the wrapper is marked `decode-error` and the island's own fallback subtree is rendered.
8. If the island panics — in its factory or anywhere in its hydration — the wrapper is marked `error`, its content is replaced with `<div role="alert">Hydration fallback rendered.</div>`, an `island_panic` diagnostic carrying the panic message is logged, and hydration **continues with the next island**. See [Panic isolation](#panic-isolation).
9. A reactive attribute (a closure-valued attribute, [ADR 0015](../adr/0015-reactive-attributes.md)) is bound to the adopted element with an effect. Its first run writes only where the DOM disagrees, so a server-rendered attribute that matches is never rewritten, and it never writes a form control's live `value`/`checked`/`selected` property during adoption, which may hold input made before the bundle loaded.
10. `<script>` and `<style>` elements are adopted without reconciling their text. The server emits their raw text with breakout sequences rewritten (`</script` as `<\/script`), so the DOM text legitimately differs from the vnode, and a script's text is not re-executed anyway.

## Entry points

Every entry point is exported to JavaScript under the same name, and every one
is panic-isolated per island.

| Call | Scope | Use it when |
|---|---|---|
| `hydrate()` | every `[data-island]` in the document | Page load. |
| `hydrate_within(root)` | `root` if it is an island, and every `[data-island]` inside it | Markup arrived after first paint — a modal, a fetched panel, a swapped router outlet. |
| `hydrate_within_selector(selector)` | every document match of `selector`, and the islands inside each | Staged hydration: critical islands now, the rest on idle. Returns how many boundaries failed in the call. |
| `hydrate_island(element)` | `element` only, not islands nested in it | Driving hydration one island at a time. Returns the boundary state it ended in, or `undefined` for a non-island. Builds the registry lookup per call, so prefer `hydrate_within_selector` for many islands. |
| `unmount(root)` | `root`'s subtree | That markup is being removed. Detaches its listeners, dynamic regions, and effects. |

`hydrate()` is idempotent: a boundary already hydrated is not hydrated twice,
so calling it again after inserting new markup costs a walk and nothing else.
This is what makes the router's swap-and-re-hydrate safe — before it held, a
second call re-registered every listener and one click fired its handler twice.
It is also what makes staged hydration a matter of *calling* the runtime in
stages, rather than hiding islands from it:

```js
import init, { hydrate_within_selector, start_router } from '/pkg/app.js';

await init();
hydrate_within_selector('[data-island]:not([data-island="Comments"])');
requestIdleCallback(() => hydrate_within_selector('[data-island="Comments"]'));
start_router();
```

Until 0.6.0 the reference frontend deferred islands by renaming `data-island`
away and stripping it again after each pass, because only `hydrate()` was
exported and a second full pass would have double-bound everything. That block
is gone.

## Panic isolation

`wasm32-unknown-unknown` is `panic = "abort"`. A panic runs the panic hook and
then traps; `catch_unwind` never returns `Err`, and until 0.6.0 the trap escaped
the whole hydration loop, so every island after a panicking one stayed at `ssr`
— unhydrated, inert, and silent.

Each island is now hydrated through a small JavaScript trampoline shipped inside
the bundle (a wasm-bindgen `inline_js` snippet, emitted under `snippets/` next to
the JS glue — serve the whole `pkg/` directory). The trampoline calls back into
wasm inside `try`/`catch`. A trap unwinds only the frames above it; the loop
below sees the failure, stamps that boundary `error`, renders the fallback, and
moves on. A panic hook, chained in front of whatever hook the application had
already installed, records the panic message for the diagnostic; if the
application replaces the hook after the first hydration, the diagnostic falls
back to the JS error text (`RuntimeError: unreachable`).

It costs one JS crossing per island and nothing else on a page with no panics.
It needs no `unsafe-eval` in a Content-Security-Policy.

The recovery is **best-effort**. The instance stays callable after a trap, but
the frames that panicked are abandoned mid-flight, without running a single
destructor: their allocations leak, and a `RefCell` they held borrowed stays
borrowed. The reactive runtime's per-thread position is *not* left behind: since
0.6.0 the hydration loop snapshots it before each island — the current owner
(context scope), the current subscriber, and the `batch` and flush depths — and
restores it after a trap, so the next island's scope is not parented to the dead
one (it would otherwise see the dead island's contexts) and an abandoned `batch`
does not defer every later signal write. Effects that batch had deferred stay
queued and run when the next batch closes. If the trap left one of those cells
itself borrowed, it cannot be restored; a `reactive_state_unrecoverable`
diagnostic says so. Releasing the failed island's resources — which disposes its
region effects and runs their `on_cleanup` callbacks — is isolated too: a panic
there is logged as `island_cleanup_panic` and hydration continues. None of this
happens unless an island panics, and the alternative it replaces lost every
later island on the page. Two build configurations defeat it: a
`panic = "unwind"` build (nightly `-Z build-std`) does not need it — unwinding
reaches `catch_unwind` directly — and wasm-bindgen's
`--force-enable-abort-handler`, which poisons the instance on the first trap,
turns a contained failure back into a dead page.

Removing hydrated markup without `unmount` leaks. The event closures, the
dynamic-region records, and the effects a region's reactivity created are owned
by the runtime, not by the DOM node, so dropping the node alone leaves all three
alive for the life of the page.

## Client-side routing

`start_router()` (exported to JS as `start_router`) installs a capturing `click`
listener and a `popstate` listener. An intercepted navigation fetches the target
URL, takes the contents of the element marked `data-krab-router-outlet` from the
response, swaps it into the live document, and re-hydrates.

```html
<body>
  <nav><a href="/about">About</a></nav>
  <main data-krab-router-outlet>
    <!-- swapped on navigation; everything outside is left alone -->
  </main>
</body>
```

A click is left to the browser when intercepting it would break an expectation
the user already has: a modifier key or a non-primary button, a `target` other
than `_self`, `download`, a cross-origin URL, an explicit
`data-krab-router-ignore` on the anchor, or an event something else already
handled. Same-page fragment links scroll rather than refetch. Every failure —
no outlet in the current page, no outlet in the response, a failed fetch — falls
back to a full browser navigation.

Start the router *after* the first hydration pass, so it can never swap out
markup that has not been claimed yet. The reference frontend
(`services/service_frontend`) does exactly that: a shared `<nav>` sits outside
the outlet on every page, the home page's islands demo sits inside it, and its
live-data panel sits below it, outside, so the panel keeps its DOM across
navigations. Pages the router navigates *to* must carry an outlet too, or the
router falls back to a full load after having already fetched them.

Router fetches carry `x-krab-router: 1`; see
[`docs/reference/api.md`](../reference/api.md) for the request contract.

Deliberately absent: nested layouts (one outlet per document), prefetching, and
a client-side route table. The server stays the router, so SSR, ISR, and render
policy remain authoritative instead of being duplicated on both sides.

## Suspense boundaries and streaming

Marker values are plain HTML attributes, so chunked SSR streaming preserves them
without additional encoding. `<Suspense>` boundaries
([ADR 0016](../adr/0016-suspense-boundaries.md)) render comment markers around
their content:

```text
<!--krab:suspense:<boundary-id>:pending-->  …fallback or children…  <!--krab:suspense:<boundary-id>:resolved-->
```

The markers are `Node::Comment` vnodes, so a boundary inside an island hydrates
like any other content: the walk adopts a DOM comment in the marker's slot
**without comparing its text** — boundary ids come from a per-process counter
that the server and the browser advance independently — and a missing marker is
inserted, never swapped in over a real node. After hydration the boundary is an
ordinary dynamic region: when its last pending resource produces a first value,
the reconciler replaces the fallback with the children between the markers.

### Progressive streaming

Under `render_to_stream` ([ADR 0017](../adr/0017-progressive-streaming-ssr.md))
a boundary whose pending resources all have server loaders is *deferred*: the
shell carries its fallback between `<!--krab:suspense:<id>:pending-->` and
`<!--/krab:suspense:<id>-->`, and the resolved content arrives later in the same
response:

```html
<template data-krab-suspense="<id>">…content…<!--krab:suspense:<id>:resolved--></template>
<span data-krab-suspense-ready="<id>" hidden></span>
```

The swap runtime (`STREAM_SWAP_SCRIPT`, served at `/_krab/stream.js`) watches for
the sentinel, replaces everything between the two markers with the template's
content, and dispatches two events on `document`, each with
`detail: { id, nodes }`: `krab:suspense-resolving` with the outgoing fallback
nodes just before it removes them, and `krab:suspense-resolved` with the
swapped-in nodes after. `hydrate()` listens for both: it calls `unmount` on each
outgoing element, so an island hydrated in the fallback releases its closures
and effects, and `hydrate_within` on each incoming one, so islands in streamed
content hydrate whether they arrive before or after the bundle has loaded
(content that arrived first is simply part of the document `hydrate()` walks).

Islands inside streamed content receive their data the ordinary way: the
content is rendered on the server *after* the loader finished, so the island's
props — and a `create_resource_with_initial` fed from them — already carry it.
A deferred boundary should sit in page markup, not inside an island: an island
is hydrated against its own client-side resource state, which starts without
the streamed data.

## Debugging workflow

When hydration looks wrong:

1. Inspect the island wrapper in the DOM and confirm `data-krab-boundary-id`, `data-krab-boundary-state`, and `data-krab-boundary-mismatches`.
2. Check browser diagnostics emitted by `krab_client`. They are machine-readable JSON and include:
   `scope`, `island`, `boundary_id`, `reason`, `detail`, and `path`.
3. If `reason` is `marker_reordered_dom_node`, the runtime recovered by moving a sibling into place.
4. If `reason` is `element_marker_mismatch` or `element_tag_mismatch`, SSR and client trees diverged for that path.
5. If the wrapper state is `patched`, the runtime recovered but the server and client trees were not identical.
6. If the wrapper state is `decode-error`, `error`, or `missing-definition`, hydration did not complete successfully for that boundary.
