# Reference Apps

Krab maintains five official reference tracks. Each track maps to a starter or service topology and shows which framework capability it exercises.

| Use case | Reference path | Start from | Exercises |
| --- | --- | --- | --- |
| Content site | `examples/reference_apps/content_site` | `krab new content-site --template default` | static routes, render policy, deployment basics |
| SaaS dashboard | `examples/reference_apps/saas_dashboard` | `krab new saas-dashboard --template saas` | auth-ready HTTP layers, tenant API scaffolding, release checks |
| Edge-rendered app | `examples/reference_apps/edge_rendered` | `krab new edge-rendered --template edge-ssr` | explicit route render policy, ISR metadata, edge eligibility |
| Event-stream app | `examples/reference_apps/event_stream` | `krab new event-stream --template event-stream` | SSE, WebSocket, readiness/liveness defaults |
| Split-service example | `examples/reference_apps/split_service` | `krab topology split users --protocols rest,graphql,rpc --register` | service contracts, adapter separation, orchestrator config |

## Selection Guide

Use the content-site track when you need the smallest deployable web service and want to understand the project model.

Use the SaaS-dashboard track when your first concern is operational defaults: auth-ready middleware, release evidence, readiness probes, and CI gates.

Use the edge-rendered track when route-level rendering policy matters. This is the closest reference to Astro/Nuxt-style render mode decisions, but expressed through Krab's Rust policy types.

Use the event-stream track for dashboards, live status pages, and data feeds that need SSE or WebSocket handlers.

Use the split-service track when the differentiator matters most: local development as one framework, with explicit service boundaries ready for remote deployment.
