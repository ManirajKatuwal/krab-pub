# Why Krab

Krab is strongest when the application is both a web experience and a service system.

## Differentiators

- Rust-native full stack: handlers, server functions, rendering policy, and service contracts live in one Rust workspace.
- Service-aware tooling: `krab.toml`, `krab bootstrap`, and the orchestrator model startup order, readiness, restarts, and logs as first-class concerns.
- Operational defaults: generated projects include `/health`, `/ready`, CI gates, container probes, security defaults, `krab doctor`, and `krab release certify`.
- Explicit render policy: route behavior is represented as `RouteRenderPolicy` rather than hidden in scattered route code.
- Hydration invariants: island SSR output carries stable boundary and node markers so hydration mismatches can be diagnosed.

## When Krab Fits

Use Krab when you want a Rust web framework that also cares about process supervision, release evidence, service boundaries, and deployment hygiene.

Use a mainstream JavaScript framework first when the highest priority is a large component ecosystem, many hosted adapter presets, or hiring from a broad frontend talent pool.

## Product Position

Krab should not compete by copying every feature from Next.js, Astro, SvelteKit, or Nuxt. Its clearer product position is:

> A Rust full-stack framework for teams that want web rendering, server functions, service orchestration, and production evidence in one coherent project model.
