# ADR 0011: ISR Key Separator and Cold-Miss Single-Flight

## Status

**Accepted** — 2026-08-12, by the repository owner. Implemented in the same
change (0.3.0).

## Context

Two related defects in the ISR cache (`krab_core::isr`) over a shared
`DistributedStore`:

**Namespace prefix over-match.** Cache keys were `"{namespace}:{path}"` and
the default namespace is `krab:isr`. The module's own documentation
recommended sub-namespaces like `krab:isr:site`. Because `invalidate_all` and
`len` enumerate `keys_with_prefix("{namespace}:")`, a cache on the default
namespace matched — and could silently wipe or miscount — every entry of any
sibling namespace that it string-prefixes. The isolation test used two
non-prefixing siblings, so it could not catch this.

**Cold-miss stampede.** `serve` was a bare get-plus-staleness check. On a cold
key (fresh deploy, post-invalidation) every concurrent request missed and
rendered. The only mitigation lived in `service_frontend` and covered *stale
revalidation* on a single process — cold misses and cross-replica duplication
were unhandled.

## Decision

1. The namespace/path join uses a separator that cannot appear in a namespace
   (`\u{1}`), so `keys_with_prefix` over one namespace can never match a
   sibling namespace's keys. Entries written under the old `:` separator are
   simply never matched again: they miss and repopulate. **This is a one-time,
   full ISR cache flush at deploy time** — accepted deliberately rather than
   migrating entries in place.
2. `DistributedStore` gains `set_if_absent(key, value, ttl)` (Redis
   `SET NX PX`; in-memory equivalent under the write lock), and the ISR cache
   uses it as a short-TTL lease for cold-miss single-flight: one caller per
   store wins the lease and renders; the others are told the miss is locked
   and fall back to rendering without populating (fail-open — a store outage
   never blocks serving).

## Consequences

- `invalidate_all`/`len` are namespace-exact. The default namespace and
  documented sub-namespace patterns can coexist on one store.
- The deploy that ships this change re-renders every ISR page once. The
  single-flight lease bounds that burst to one render per path per store.
- `DistributedStore` grew a required method — external implementations of the
  trait must add `set_if_absent` (breaking, part of the 0.3.0 major-surface
  changes).
