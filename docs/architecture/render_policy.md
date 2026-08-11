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
> let runtime = RuntimeState::new();               // reads KRAB_REDIS_URL
> let isr_cache = IsrCache::with_store(runtime.store.clone());
> ```
>
> `service_frontend` does exactly this, so setting `KRAB_REDIS_URL` is all that
> is needed there. See [`docs/reference/environment.md`](../reference/environment.md).

- `CacheMode`: `None`, `Static`, `Isr`, or `Swr`
- `EdgeCapability`: `OriginOnly`, `Eligible`, `Preferred`, or `Required`
- `streaming`: whether the route may emit streamed SSR output. **What
  "streamed" means today:** the render is synchronous; `ChunkedStreamWriter`
  splits the finished output into chunks and stamps suspense markers
  (`<!--krab:suspense:…-->`) that no client code yet consumes. This is chunked
  delivery of a complete render — not progressive or out-of-order rendering,
  which was explicitly deferred by
  [ADR 0009](../adr/0009-resource-ssr-semantics.md)

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
