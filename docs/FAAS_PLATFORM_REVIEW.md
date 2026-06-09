# Krab FaaS / Backend Platform Review

Date: 2026-06-10

This document captures a product, technical, and market review of Krab as a potential Framework-as-a-Service / backend platform similar in category to Firebase, Supabase, or Vercel.

## Executive Summary

Krab is not yet a hosted Firebase/Supabase/Vercel competitor. It is currently closer to a Rust-native full-stack framework with serious production-oriented scaffolding: SSR/islands, server functions, service composition, protocol flexibility, migration governance, diagnostics, security policy, and local orchestration.

The strongest path is not to clone Firebase or Supabase. The strongest path is to narrow into:

> Rust-native full-stack cloud for high-performance SSR, typed service contracts, and production governance.

The framework foundation is credible. The platform story is not yet credible until Krab has a real control plane, isolated runtime workers, distributed state, deployment workflow, observability, metering, and multi-tenant isolation.

Highest-impact next step:

Build a hosted deployment MVP around `krab deploy`, immutable app revisions, isolated runtime workers, Redis-backed runtime state, object-backed static/ISR storage, logs, metrics, and basic usage metering.

## Priority Findings

| Priority | Finding | Impact |
| --- | --- | --- |
| Critical | Browser server-function CSRF handling appears inconsistent. The CSRF endpoint sets `krab_csrf_token` as `HttpOnly`, while the WASM client searches for `csrf_token` in `document.cookie`. | CSRF-protected server-function calls can fail or provide a false sense of security. |
| Critical | Refresh-token replay detection is check-then-set rather than atomic. | Concurrent refresh requests can both pass unless the distributed store supports atomic `SET NX` / compare-and-set behavior. |
| High | Production fallback to process-local memory store is dangerous. | Rate limits, auth revocation, refresh replay state, and other distributed runtime state become per-process. |
| High | ISR cache and WebSocket rooms are process-local. | Horizontal scaling creates inconsistent cache behavior and disconnected realtime rooms. |
| High | Migration drift detection is logged in the service lifecycle, but drift enforcement is not called there. | Governance claims can diverge from actual production behavior. |
| High | Framework, demo app, runtime policy, and platform concerns are still mixed. | Hosted SaaS development will become harder unless control-plane and runtime responsibilities are separated. |
| Medium | Cache middleware buffers full responses before storage. | Large responses increase memory pressure and latency. |
| Medium | Local orchestration is useful for development but is not a hosted scheduler. | It cannot provide multi-tenant deployment, health, isolation, region placement, or rollout guarantees. |

## 1. Current Architecture Review

### What is working

Krab uses several good architectural patterns:

- Cargo workspace separation across framework, macros, client, server, CLI, orchestrator, and services.
- Axum/Tower middleware layering for HTTP policy.
- Typed domain and repository abstractions in `service_users`.
- Protocol adapter separation across REST, GraphQL, and RPC.
- Capability reporting for services.
- Migration governance primitives.
- CLI diagnostics and topology validation.
- Local process supervision for development workflows.

The strongest architectural area is the user service split between domain logic, repository implementations, REST/GraphQL/RPC adapters, and runtime app assembly. That pattern is suitable for a platform where services may be generated, deployed, inspected, and evolved independently.

### What is not yet platform-grade

Krab still mixes several layers that a hosted platform needs to separate:

- Application framework concerns.
- Demo/product application code.
- Runtime middleware policy.
- Deployment and environment policy.
- Hosted platform control-plane policy.

For example, runtime state currently owns store selection, auth middleware behavior, rate limiting, metrics, CORS, CSRF, and service-auth configuration. That is acceptable for a framework runtime, but a hosted platform needs the control plane to own those decisions per organization, project, deployment, region, tenant, and pricing plan.

### Scalability assessment

Compute can scale horizontally if the application is stateless and a distributed store is configured. However, several important features remain process-local:

- ISR cache.
- WebSocket room manager.
- Memory-store fallback.
- In-process metrics buckets.
- Local orchestration state.

These are reasonable for local development and early framework testing. They are not sufficient for hosted multi-replica or multi-region deployments.

## 2. Performance and Rust Usage

### Rust advantages currently used well

Krab benefits from Rust in the following areas:

- Strong typing around HTTP, server functions, service contracts, and configuration.
- Memory safety without garbage collection.
- Tokio/Axum async runtime integration.
- SQLx-backed database access.
- Compile-time macros for framework ergonomics.
- Explicit error handling and policy validation.
- Good test and clippy discipline.

The codebase shows an engineering culture oriented toward correctness and operational checks, which is a meaningful Rust-aligned advantage.

### Rust advantages not fully exploited yet

The current system is not yet a clear performance moat. Some paths still rely on:

- Dynamic dispatch for framework nodes, callbacks, domain services, repositories, and store interfaces.
- JSON envelopes for server functions and protocol adapters.
- Boxed futures in some abstraction layers.
- Whole-response buffering in cache middleware.
- Process-local locks in async-adjacent runtime paths.
- Runtime-generated or inline JavaScript for hydration.

These choices are not automatically wrong. They are pragmatic. But if Krab is positioned as performance-first, the project needs benchmarks proving that the abstraction cost is lower than competing systems.

### Likely latency and overhead risks

High-risk areas:

- ISR/cache response buffering.
- Redis operation overhead if connections are not pooled or pipelined.
- WASM hydration startup and island prop serialization.
- Server-function dispatch through JSON.
- WebSocket room locks and local fanout.
- Fallback process-local state under multi-replica deployment.

Recommended benchmarks:

- Cold SSR latency.
- Warm SSR latency.
- Island hydration startup time.
- Server-function p50/p95/p99 latency.
- Cache hit/miss latency.
- WebSocket fanout throughput.
- Memory usage under cached large responses.
- DB pool saturation behavior.

## 3. Developer Experience

### Current DX strengths

Krab already has promising developer-facing primitives:

- `view!` macro.
- `#[island]` macro.
- `#[server]` macro.
- Starter templates.
- `krab_cli doctor`.
- Topology and service checks.
- Protocol flexibility.
- Migration policy tools.
- Test coverage across framework and services.

These make Krab feel like a real framework rather than a collection of libraries.

### Missing compared with mature frameworks

Compared with Next.js, NestJS, Django, Rails, Laravel, and similar ecosystems, Krab still lacks:

- File-based or manifest-driven routing.
- First-class forms, validation, redirects, sessions, and error boundaries.
- Complete auth provider integrations.
- Clear authorization patterns.
- Simple database/schema workflow for application developers.
- Local emulator/dev dashboard.
- One-command deploy.
- Preview deployments.
- Asset pipeline and styling conventions.
- Component development workflow.
- Testing story for full-stack user flows.
- Plugin/module ecosystem.
- Extensive docs and examples for real applications.

The current templates are transparent about this gap. The SaaS template does not yet include completed auth, database schema, or multi-tenant persistence. Generated remote-service scaffolds are also not marked as remote-ready.

## 4. FaaS / SaaS Readiness

Krab can become a hosted platform, but the hosted platform should be designed as a separate product layer.

### Required control plane

A hosted Krab control plane needs:

- Organizations.
- Projects.
- Environments.
- Tenants.
- Deployments.
- Immutable revisions.
- Build queue.
- Artifact registry.
- Deploy history.
- Rollback.
- Secrets.
- Environment variables.
- Domains.
- TLS automation.
- Region placement.
- Billing.
- Metering.
- Quotas.
- Audit logs.
- Migration approvals.
- Release policy.
- Observability.

### Required runtime plane

A hosted Krab runtime plane needs:

- Isolated app workers.
- Per-deployment routing.
- Distributed cache/session/rate-limit store.
- Region-aware service discovery.
- Object storage for artifacts, static assets, and ISR.
- Centralized logs and metrics.
- Background jobs and queues.
- Health checks and rollout automation.

The runtime isolation strategy should be chosen deliberately:

- Containers are the simplest MVP path.
- Kubernetes is practical if operational complexity is acceptable.
- Firecracker provides stronger isolation but raises complexity.
- Wasmtime can be attractive for plugin/function isolation, but Rust full-stack apps may not fit cleanly without constraints.

### Multi-tenancy requirements

Multi-tenancy is not just tenant IDs in claims or request paths. Krab needs:

- Organization/project/tenant data model.
- Project-scoped auth and API keys.
- Per-tenant and per-project quotas.
- Per-project rate limits, not just IP limits.
- Secret isolation.
- Deployment isolation.
- Audit trails.
- Billing attribution.
- Data partitioning strategy.
- Tenant-aware observability.

Data isolation needs an explicit product decision:

- Row-level isolation is cheapest and fastest to operate.
- Schema-per-tenant improves separation but increases migration complexity.
- Database-per-tenant improves isolation but increases operational cost.

## 5. Competitive Positioning

Krab does not currently compete directly with Firebase, Supabase, or Vercel.

Firebase is a broad backend/app platform including auth, functions, storage, hosting, databases, app hosting, and related Google Cloud integrations.

Supabase is a Postgres-centered backend platform with auth, database APIs, realtime, storage, edge functions, row-level security, backups, poolers, logs, and analytics.

Vercel is strongest in frontend deployment, serverless/functions, CDN, preview deployments, CI/CD, and global runtime infrastructure.

Krab should not try to match all of that surface area early. The product would become too broad before any single area is excellent.

### Strongest differentiation angle

Recommended positioning:

> Krab Cloud is a Rust-native full-stack app platform for high-performance SSR, typed server functions, service contracts, and production governance.

That is more believable than "Firebase in Rust" and more focused than a generic backend platform.

### What Krab can credibly own

Krab can credibly own:

- Rust-native full-stack development.
- SSR/islands with lower runtime overhead.
- Typed server functions.
- Service contracts and protocol adapters.
- Production-readiness checks built into the framework.
- Migration/release governance.
- Secure-by-default deployment policy.

Krab should avoid trying to own everything at once:

- General-purpose database platform.
- Full auth provider ecosystem.
- Object storage platform.
- Global CDN.
- Analytics suite.
- General serverless compute platform.

## 6. Scaling Strategy

### What breaks at 10x

Likely breakpoints:

- DB pool exhaustion.
- Large response buffering in cache middleware.
- Process-local ISR cache inconsistency.
- Process-local WebSocket rooms.
- Memory-store fallback.
- Per-process rate limits.
- Insufficient backpressure under burst traffic.

### What breaks at 100x

At this stage Krab needs externalized state and platform primitives:

- Redis/Valkey/Dragonfly for distributed runtime state.
- Postgres pooler.
- Read replicas.
- Queue workers.
- Object storage.
- CDN.
- Distributed logs.
- Distributed tracing.
- Project-level quotas.
- Load testing and capacity modeling.

### What breaks at 1000x / global

At global scale Krab needs:

- Multi-region runtime placement.
- Global routing.
- Artifact replication.
- Regional data policy.
- Distributed cache invalidation.
- Canary releases.
- Automated rollback.
- Idempotent migrations.
- Region-aware service discovery.
- Control-plane reliability independent of app runtime reliability.

### Redesign principles

Krab should move toward:

- Stateless app runtimes.
- Immutable deployments.
- Production-disabled memory fallback.
- Distributed runtime state.
- Object-backed static and ISR assets.
- Explicit project and tenant metering.
- Control-plane-owned runtime policy.

## 7. Monetization Potential

Krab can become a SaaS product if it sells hosted operational value, not just the framework.

### Free tier candidates

- Open-source framework and CLI.
- One hobby project.
- Small compute quota.
- Small build-minutes quota.
- One region.
- Basic logs and metrics.
- Community support.

### Paid tier candidates

- Compute usage.
- Bandwidth.
- Build minutes.
- Storage.
- Team seats.
- Custom domains.
- Preview deployments.
- Secrets management.
- Longer log and trace retention.
- Managed Redis/Postgres/object-storage integrations.
- Migration governance.
- Release approvals.
- SSO/SAML.
- Audit logs.
- Compliance exports.
- SLA and support.

### Pricing direction

Avoid charging for the framework early. The framework should drive adoption. Charge for hosting, collaboration, operational safety, scale, governance, and managed infrastructure.

Recommended first paid plan:

- Team plan.
- Per-seat base.
- Included compute/build/log quota.
- Usage-based overages.
- Custom domains.
- Preview deployments.
- Secrets.
- Longer log retention.

## 8. Final Verdict

Continue building, but narrow the scope.

Do not continue as a generic Firebase/Supabase/Vercel clone. That path is too broad and will dilute engineering effort.

Recommended product direction:

> Krab Cloud: hosted Rust full-stack apps with SSR/islands, typed server functions, service composition, deployment governance, observability, and safe production defaults.

### Highest-impact next step

Build a deployable hosted MVP:

1. `krab deploy` uploads a repo, build context, or container artifact.
2. Control plane stores project, environment, deployment, revision, secret, domain, and region metadata.
3. Build worker creates an immutable artifact.
4. Runtime worker runs one isolated app revision.
5. Router sends traffic to the active revision.
6. Runtime state uses Redis or equivalent, not process memory.
7. Static assets and ISR artifacts use object storage.
8. Logs and metrics are collected centrally.
9. Basic usage metering is recorded per project.

### Immediate technical blockers to fix first

- Fix CSRF token name and browser access model for server functions.
- Replace refresh-token replay check-then-set with atomic store semantics.
- Enforce migration drift policy in the production lifecycle.
- Make production memory-store fallback explicit, opt-in, or disabled.
- Define a distributed ISR/cache strategy.
- Define a cross-replica WebSocket/realtime strategy.
- Separate control-plane policy from runtime app state.

## Code Evidence Map

Representative areas reviewed:

- `crates/framework/krab_core/src/lib.rs`: core node/render/hydration primitives.
- `crates/framework/krab_core/src/signal.rs`: signal runtime and single-threaded reactive state.
- `crates/framework/krab_core/src/http.rs`: common HTTP middleware layering.
- `crates/framework/krab_core/src/http_runtime.rs`: runtime state, store initialization, metrics.
- `crates/framework/krab_core/src/store.rs`: memory and Redis distributed-store implementations.
- `crates/framework/krab_core/src/http_security.rs`: CSRF token endpoint and middleware.
- `crates/framework/krab_core/src/server_fn.rs`: server-function registration and dispatch.
- `crates/framework/krab_core/src/isr.rs`: in-process ISR cache.
- `crates/framework/krab_core/src/ws.rs`: in-process WebSocket room manager.
- `crates/framework/krab_core/src/db.rs`: migration governance and drift policy primitives.
- `crates/framework/krab_macros/src/lib.rs`: `#[server]`, `#[island]`, and `view!` macros.
- `crates/tooling/krab_cli/src/project_template.rs`: starter templates and stated template limitations.
- `crates/tooling/krab_orchestrator/src/main.rs`: local service supervision.
- `services/service_auth/src/main.rs`: auth routes and refresh-token flow.
- `services/service_frontend/src/app_state.rs`: frontend runtime state.
- `services/service_frontend/src/cache.rs`: cache middleware and ISR integration.
- `services/service_frontend/src/main.rs`: SSR, hydration runtime, and app handlers.
- `services/service_frontend/src/protocol_client.rs`: protocol-aware client and fallback behavior.
- `services/service_users/src/domain/service.rs`: domain-service abstraction.
- `services/service_users/src/db/bootstrap.rs`: repository selection.
- `services/service_users/src/db/migrations.rs`: service migration lifecycle.
- `services/service_users/src/runtime.rs`: protocol adapters and app assembly.

## External Market References To Recheck

Use official product pages when updating this document:

- Firebase products and pricing.
- Supabase features and pricing.
- Vercel functions, CDN/network, and pricing.

Market positioning changes quickly. Re-check these sources before using this review for fundraising, customer-facing positioning, or pricing decisions.
