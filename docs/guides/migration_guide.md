# Migration Guide

This guide maps common framework concepts to Krab equivalents. It is intentionally conceptual; use the reference apps for concrete layouts.

## From Axum

| Axum concept | Krab equivalent |
| --- | --- |
| `Router` and handlers | Keep Axum handlers, then add Krab HTTP layers, service config, and release checks |
| Manual health routes | Standard `/health` and `/ready` routes in generated starters |
| Local process scripts | `krab bootstrap` with supervised startup order and readiness checks |
| Ad hoc CI | `krab doctor` and `krab release certify` evidence bundles |

Recommended path:

1. Move existing routes behind a Krab project model in `krab.toml`.
2. Add `/health` and `/ready`.
3. Apply common HTTP layers and runtime state.
4. Add release certification to CI before changing behavior.

## From Leptos

| Leptos concept | Krab equivalent |
| --- | --- |
| Server functions | `#[server]` functions mounted under `/api/rpc/{name}` |
| Islands | Krab islands with explicit hydration markers |
| SSR policy | `RouteRenderPolicy` with `RenderMode` and `CacheMode` |
| Full-stack app shell | Krab service plus orchestrator and release tooling |

Key difference: Krab treats server functions as public HTTP endpoints and documents validation/auth responsibilities explicitly.

## From Next.js, Astro, SvelteKit, or Nuxt

| Concept | Krab equivalent |
| --- | --- |
| API routes / server actions | `#[server]` functions or Axum handlers |
| Route rendering modes | `RouteRenderPolicy` |
| Islands / partial hydration | Krab island components with server-emitted hydration markers |
| Platform adapters | Rust service deployment plus orchestrator config |
| Preview/deploy checks | `krab doctor` and `krab release certify` |

Krab is not trying to mirror every frontend convention. The trade is Rust-native service composition, explicit operations defaults, and one project model for local development through release evidence.
