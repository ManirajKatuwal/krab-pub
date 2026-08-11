# ADR 0001: Hydration Markers

## Status

Accepted

## Context

Island hydration cannot rely only on DOM position. Reordered siblings, nested islands, streamed fragments, and browser DOM normalization can make positional matching ambiguous.

## Decision

Server-rendered island output carries explicit boundary and node metadata:

- `data-krab-boundary`
- `data-krab-boundary-id`
- `data-krab-boundary-state`
- `data-krab-node-id`

The client hydrator prefers explicit markers before falling back to positional reconciliation.

## Consequences

- Hydration mismatches are diagnosable with machine-readable metadata.
- SSR output is slightly more verbose.
- Tests must preserve marker format across stream and nested-island changes.
