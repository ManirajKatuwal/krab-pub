# Hydration Invariants

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
