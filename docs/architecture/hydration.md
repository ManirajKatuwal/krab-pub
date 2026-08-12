# Hydration Invariants

## Building a bundle that hydrates

The runtime lives behind `krab_client`'s `web` feature — `hydrate()`, the
reconciler, the router's browser half, all of it. `web` is a **default** feature
as of 0.3.0. Before that it was opt-in, and a build that omitted it still
produced a valid, loadable module: `hydrate()` logged one line and returned. It
was ~15 KB against ~198 KB for the real runtime, and nothing on the page
reported the difference.

```sh
wasm-pack build crates/framework/krab_client --release --target web -- --features web
```

Naming the feature is redundant now and is written out anyway, because the
failure it prevents is silent. If you need to check an artifact you did not
build, the runtime contains the string `Hydrating island` and a stub does not:

```sh
grep -a "Hydrating island" dist/pkg/krab_client_bg.wasm
```

The **server** side wants the opposite. `#[island]` selects its SSR half on
`not(feature = "web")` — that half is what emits the `data-island` /
`data-props` wrapper the browser later finds. A server crate therefore depends
on `krab_client` with `default-features = false`; with `web` on, the same call
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
7. If the island factory panics or props fail to decode, the wrapper is marked with an error state and a fallback subtree is rendered.

## Entry points

| Call | Scope | Use it when |
|---|---|---|
| `hydrate()` | every `[data-island]` in the document | Page load. Exported to JS as `hydrate`. |
| `hydrate_within(root)` | `[data-island]` inside `root` | Markup arrived after first paint — a modal, a fetched panel, a swapped router outlet. Rust only. |
| `unmount(root)` | `root`'s subtree | That markup is being removed. Detaches its listeners, dynamic regions, and effects. Rust only. |

`hydrate()` is idempotent: a boundary already hydrated is not hydrated twice,
so calling it again after inserting new markup costs a walk and nothing else.
This is what makes the router's swap-and-re-hydrate safe — before it held, a
second call re-registered every listener and one click fired its handler twice.

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

Router fetches carry `x-krab-router: 1`; see
[`docs/reference/api.md`](../reference/api.md) for the request contract.

Deliberately absent: nested layouts (one outlet per document), prefetching, and
a client-side route table. The server stays the router, so SSR, ISR, and render
policy remain authoritative instead of being duplicated on both sides.

## Streaming compatibility

Marker values are plain HTML attributes, so chunked SSR streaming preserves them without additional encoding. Suspense boundaries continue to use comment markers:

```text
<!--krab:suspense:<boundary-id>:pending-->
<!--krab:suspense:<boundary-id>:resolved-->
```

This means streamed HTML can contain both:

- boundary/node identity via `data-krab-boundary-id` and `data-krab-node-id`
- stream progress via `krab:suspense` comments

## Debugging workflow

When hydration looks wrong:

1. Inspect the island wrapper in the DOM and confirm `data-krab-boundary-id`, `data-krab-boundary-state`, and `data-krab-boundary-mismatches`.
2. Check browser diagnostics emitted by `krab_client`. They are machine-readable JSON and include:
   `scope`, `island`, `boundary_id`, `reason`, `detail`, and `path`.
3. If `reason` is `marker_reordered_dom_node`, the runtime recovered by moving a sibling into place.
4. If `reason` is `element_marker_mismatch` or `element_tag_mismatch`, SSR and client trees diverged for that path.
5. If the wrapper state is `patched`, the runtime recovered but the server and client trees were not identical.
6. If the wrapper state is `decode-error`, `error`, or `missing-definition`, hydration did not complete successfully for that boundary.
