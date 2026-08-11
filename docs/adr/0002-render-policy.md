# ADR 0002: Route Render Policy

## Status

Accepted

## Context

Frameworks such as Nuxt and SvelteKit make route rendering behavior visible at the route boundary. Krab previously encoded cache and render behavior through route-specific conditionals.

## Decision

Represent route behavior with `RouteRenderPolicy`, `RenderMode`, `CacheMode`, and `EdgeCapability`.

Routes should attach policy data explicitly, and cache authority should be derived from the policy rather than a hardcoded route list.

## Consequences

- Route behavior becomes inspectable and testable.
- Starters can describe SSR, ISR, SWR, static, and edge eligibility with one vocabulary.
- Unsupported combinations must fail validation instead of silently degrading.
