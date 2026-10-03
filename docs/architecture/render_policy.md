# Render Policy

Krab route behavior is expressed with `RouteRenderPolicy`:

- `RenderMode`: `Static`, `Server`, or `ClientOnly`
> **ISR requires a shared store when you run more than one replica.**
> `IsrCache::new()` is backed by an in-process `MemoryStore`. Under multiple
> replicas each process holds its own copy, and invalidating a path clears
> exactly one of them — so a client refreshing sees old and new content
> depending on which instance answers. Build it over the shared store instead:
>
> ```rust,ignore
> let runtime = RuntimeState::try_new()?;           // reads KRAB_REDIS_URL, fails closed outside dev
> let isr_cache = IsrCache::with_store(runtime.store.clone());
> ```
>
> `service_frontend` does exactly this, so setting `KRAB_REDIS_URL` is all that
> is needed there. See [`docs/reference/environment.md`](../reference/environment.md).

- `CacheMode`: `None`, `Static`, `Isr`, or `Swr`
- `EdgeCapability`: `OriginOnly`, `Eligible`, `Preferred`, or `Required`
- `streaming`: whether the route may emit streamed SSR output. Two kinds of
  streaming exist, and the flag is advisory for both — nothing reads it to
  pick one:
  - **Chunked delivery of a finished render.** `ChunkedStreamWriter` splits the
    completed output into byte-budgeted chunks. The home page (`/`) does this.
  - **Progressive, out-of-order streaming** (since 0.6.0,
    [ADR 0017](../adr/0017-progressive-streaming-ssr.md)).
    `krab_core::render_stream::render_to_stream` flushes the page shell with
    `<Suspense>` fallbacks first, runs the pending resources' server loaders,
    and streams each boundary's resolved content as a `<template>` that the
    swap runtime (`/_krab/stream.js`) moves into place. `service_frontend`'s
    `/streaming` route demonstrates it.

  **Caching a streamed route.** A progressively streamed response is only
  cacheable once complete. `is_finalized_ssr_snapshot` returns `false` for any
  prefix that still has a deferred boundary open, and `true` for the finished
  body (a boundary that timed out is closed with an `error` marker). The
  frontend's cache middleware buffers a cacheable response (up to
  `KRAB_CACHE_MAX_BODY_BYTES`) and checks finalization before an ISR store, so
  it can never store a half-streamed page — but buffering means a cache *miss*
  on an ISR route loses the early flush. Give a progressively streamed route
  `CacheMode::None` (or no policy, as `/streaming` has) to keep its time to
  first byte; use ISR only if serving the finished page from cache matters more.

## Current frontend examples

The frontend service now maps representative routes to explicit policies:

- `/`
  `Server + Isr + EdgeCapability::Eligible + streaming`
- `/about`, `/greet`, `/blog/:slug`
  `Server + Isr + EdgeCapability::Eligible`
- `/data/dashboard`, `/rpc/version`
  `Server + Swr + EdgeCapability::Eligible`
- `/robots.txt`, `/sitemap.xml`, `/asset-manifest.json`
  `Static + Swr + EdgeCapability::Eligible`
- `/api/status`, `/rpc/now`
  `Server + None`
- `/streaming` has no policy (so the cache middleware passes it through
  unbuffered); it is the progressive-streaming demo

## Validation rules

The current policy validator rejects combinations that are internally contradictory:

- `Static + Isr`
- `Static + streaming`
- `ClientOnly + Isr`
- `ClientOnly + Swr`
- `ClientOnly + streaming`

`Static + Swr` is allowed because the response can be deterministic while still being served through a stale-while-revalidate cache layer.

## Why this matters

The goal is to stop scattering rendering and caching behavior across:

- route-name conditionals
- cache middleware special cases
- template marketing text

and instead keep one inspectable policy vocabulary that can be reused by services, templates, and docs.
