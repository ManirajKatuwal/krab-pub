# Reference Apps

`examples/reference_apps/` holds two different kinds of thing. The distinction
matters, so it is stated first.

## One vendored application

[`islands_rpc`](../../examples/reference_apps/islands_rpc/) is **real code**: a
workspace member that compiles, tests, and builds a WASM bundle in CI. It is the
end-to-end demonstration of Krab's core pitch — `view!` for all markup,
`#[island]` hydration, and a `#[server]` function called from an island's event
handler, all from one source file.

```sh
cargo run  -p reference_app_islands_rpc --bin islands_rpc_server   # http://127.0.0.1:3100
cargo test -p reference_app_islands_rpc
```

Start here if you want to see the framework working before deciding anything.

## Five generated tracks

The other five directories are **guides, not vendored applications.** Each holds
a walkthrough README — what to generate, what to inspect, and where to extend.
The application code is produced by the `krab new` / `krab topology split`
command in the "Generate with" column, so it always matches the current
templates. Those five are not compiled by CI; the `krab new` output they
describe is separately gated by
[`generated-project.yaml`](../../.github/workflows/generated-project.yaml), and
the services that are built and gated live in [`services/`](../../services/).

| Use case | Track guide | Generate with | Exercises |
| --- | --- | --- | --- |
| **Islands + RPC** | **`examples/reference_apps/islands_rpc` — vendored code, no generation step** | `krab new fullstack-app --template fullstack` | **`view!`, `#[island]`, `#[server]`, SSR + hydration end to end** |
| Content site | `examples/reference_apps/content_site` | `krab new content-site --template default` | static routes, render policy, deployment basics |
| SaaS dashboard | `examples/reference_apps/saas_dashboard` | `krab new saas-dashboard --template saas` | auth-ready HTTP layers, tenant API scaffolding, release checks |
| Edge-rendered app | `examples/reference_apps/edge_rendered` | `krab new edge-rendered --template edge-ssr` | explicit route render policy, ISR metadata, edge eligibility |
| Event-stream app | `examples/reference_apps/event_stream` | `krab new event-stream --template event-stream` | SSE, WebSocket, readiness/liveness defaults |
| Split-service example | `examples/reference_apps/split_service` | `krab topology split users --protocols rest,graphql,rpc --register` | service contracts, adapter separation, orchestrator config |

The split-service track has a compiled counterpart in
[`services/service_users_split`](../../services/service_users_split/): a working REST +
GraphQL split over one domain contract, built and tested by CI. Read it alongside the
track guide when you want the finished shape rather than the generated scaffold.

## Selection Guide

Use the content-site track when you need the smallest deployable web service and want to understand the project model.

Use the SaaS-dashboard track when your first concern is operational defaults: auth-ready middleware, release evidence, readiness probes, and CI gates.

Use the edge-rendered track when route-level rendering policy matters. This is the closest reference to Astro/Nuxt-style render mode decisions, but expressed through Krab's Rust policy types.

Use the event-stream track for dashboards, live status pages, and data feeds that need SSE or WebSocket handlers.

Use the split-service track when the differentiator matters most: local development as one framework, with explicit service boundaries ready for remote deployment.
