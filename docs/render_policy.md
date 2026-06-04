# Render Policy

Krab route behavior is expressed with `RouteRenderPolicy`:

- `RenderMode`: `Static`, `Server`, or `ClientOnly`
- `CacheMode`: `None`, `Static`, `Isr`, or `Swr`
- `EdgeCapability`: `OriginOnly`, `Eligible`, `Preferred`, or `Required`
- `streaming`: whether the route may emit streamed SSR output

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
