# Krab Framework: Strategic Implementation Plan (v2)

> **Last revised:** 2026-03-17  
> **Changelog:**
>
> - v2.1 (2026-03-17) — Added implementation task lists for Workstreams C–F, documented `krab_orchestrator`, marked `topology report` status, added Related Documents section.
> - v2.0 — Initial v2 plan with decision boundaries, SLOs, phased delivery, risk register, and compatibility strategy.

This version is stronger than the previous plan because it adds:

1. clear decision boundaries (what is mandatory vs optional),
2. measurable success criteria (SLOs, budgets, adoption KPIs),
3. phased delivery with ownership by crate/workstream,
4. explicit risk register and fallback paths,
5. compatibility strategy for real-world migration.

---

## 0) Product Positioning and Scope

### Positioning

Krab should be positioned as a **Rust-native full-stack platform** optimized for:

- high-throughput SSR,
- strict type-safety across boundaries,
- enterprise operations by default,
- progressive scaling from monolith to distributed services.

### Non-Goals (for first major cycle)

- Replacing every JS ecosystem tool immediately.
- Forcing microservices on all teams.
- Chasing extreme frontend animation use-cases where JS remains better.

---

## 1) Architecture Guardrails (Must-Have Invariants)

1. **Monolith-first default**: one process, one command, fast local startup.
2. **Transport-agnostic service boundaries**: same trait contract for in-process and remote calls.
3. **Streaming SSR-first**: HTML starts flowing before all data is resolved.
4. **Selective hydration**: interactive units only; no page-wide hydration by default.
5. **Operational safety defaults**: tracing, metrics, secrets policy, and dependency policy enabled by default.

---

## 2) Workstream A — Flexible Scaling (Monolith → Microservices)

### Goal

Allow teams to start as a single binary and migrate to service boundaries with near-zero application rewrite.

### Detailed Implementation Plan

#### A.1 Architectural Contract (Source of Truth)

1. Define domain contracts in `krab_core` as transport-agnostic traits:
   - `UsersService`, `AuthService`, `BillingService`, etc.
   - Request/response DTOs plus typed domain errors.
2. Prohibit direct imports across service domains except through contracts.
3. Keep all serialization formats and API version metadata attached to the contract layer.

**Concrete crate mapping**

- `crates/framework/krab_core/src/service.rs`: core trait definitions.
- `crates/framework/krab_core/src/protocol.rs`: envelope/version fields.
- `crates/framework/krab_core/src/error_boundary.rs`: shared typed error conversion strategy.

#### A.2 Runtime Adapters (Local + Remote)

Implement two adapters per domain contract:

1. **Local adapter (monolith mode)**
   - Direct in-process calls via `Arc<dyn ServiceTrait + Send + Sync>`.
   - No network serialization overhead.
   - Shared tracing context preserved as in-memory span parents.

2. **Remote adapter (distributed mode)**
   - HTTP/JSON initially (gRPC optional in phase 2).
   - Retry policy with jitter (`max_retries`, backoff cap, non-retryable status set).
   - Deadline propagation and request-scoped circuit-breaker integration.
   - Trace header injection (`traceparent`, request-id).

**Implementation rule**: both adapters must satisfy the same Rust trait signature so callers never branch on topology.

#### A.3 Topology Wiring Strategy

Use explicit topology selection at compile-time and runtime:

1. Cargo feature controls:
   - `default` => monolith wiring.
   - `distributed` => remote adapter wiring.
2. Runtime env allows endpoint overrides in distributed mode.
3. Gateway/service routes remain identical across both modes.

#### A.4 Code Generation and CLI Automation

Add tooling to avoid manual drift:

1. `krab_cli topology doctor`
   - Detect direct domain-to-domain imports bypassing contracts.
   - Detect non-serializable contract payload types.
   - Detect missing timeout/retry configuration on remote adapters.

2. `krab_cli topology split <domain>`
   - Generate service skeleton (`/health`, `/ready`, tracing middleware, config loader).
   - Generate adapter stubs and contract conformance tests.
   - Register domain in orchestrator/deployment manifests.

3. `krab_cli topology report` _(not yet implemented — planned for Phase 2)_
   - Emit current dependency graph and coupling hotspots.
   - Highlight “monolith-safe only” modules blocking extraction.

#### A.5 Migration Workflow (Monolith → Distributed)

1. Baseline monolith SLOs (latency/error/memory).
2. Extract one domain at a time (start with lowest coupling).
3. Run dual-topology test matrix in CI:
   - monolith profile
   - distributed profile
4. Compare regression budgets before promotion.
5. Roll out via canary and automated rollback triggers.

#### A.6 Testing and Quality Gates

1. Contract conformance tests:
   - Same tests run against local adapter and remote adapter.
2. Compatibility tests:
   - Verify additive schema changes do not break older callers.
3. Failure-injection tests:
   - timeouts, partial outages, retry storms, invalid responses.
4. Performance guardrails:
   - per-endpoint p95/p99 regression budget enforced in CI.

### Acceptance Criteria

- Same end-to-end suite passes in both monolith and distributed modes.
- Public API payloads and response semantics are topology-invariant.
- p95 latency regression from monolith→distributed stays within approved endpoint budgets.
- One reference domain can be extracted and rolled back safely without caller code rewrite.

### A.7 Implementation Task List (Execution Order)

- [x] Create contract-first domain interfaces in `krab_core` for users/auth.
- [x] Add topology primitives (`monolith`/`distributed`) and runtime resolver.
- [x] Implement dual adapters for one pilot domain (users): local + remote.
- [x] Add CI matrix jobs for monolith and distributed profiles.
- [x] Build `krab_cli topology doctor` checks for illegal direct imports.
- [x] Build `krab_cli topology split <domain>` scaffolder for extraction.
- [x] Add failure-injection tests for timeout/retry/circuit-breaker behavior.
- [ ] Ship first domain extraction canary with rollback playbook.

---

## 3) Workstream B — Ultra-Fast SSR with Streaming

### Goal

Compete with modern SSR frameworks by reducing TTFB and improving visual completion under load.

### Detailed Implementation Plan

#### B.1 Streaming Render Pipeline

Implement a deterministic 4-stage pipeline:

1. **Stage 0: Request planning**
   - Resolve route metadata, locale, cache authority, and render budget.
2. **Stage 1: Shell flush**
   - Immediately emit doctype, head/meta, critical CSS, and top shell.
3. **Stage 2: Async boundary streaming**
   - Emit fallback placeholders for unresolved segments.
4. **Stage 3: Progressive resolution**
   - Stream resolved fragments in boundary-id order and patch placeholders.

#### B.2 Suspense Boundary Protocol

Define a stable protocol for server/client patching:

1. Boundary ids are deterministic per render tree path.
2. Server emits markers:
   - `pending(boundary_id)`
   - `resolved(boundary_id, html_fragment)`
   - `error(boundary_id, fallback_fragment)`
3. Client hydration runtime applies patches idempotently.
4. Nested boundaries resolve independently without blocking parent shell delivery.

#### B.3 Backpressure, Cancellation, and Resource Safety

1. Use bounded channel sizes for chunk queues.
2. Detect client disconnect and cancel unresolved render futures.
3. Add per-request max render duration and max chunk count.
4. Emit structured warnings for budget breaches and cancellation causes.

#### B.4 ISR + Streaming Integration Rules

1. Cache only finalized render snapshots.
2. Never cache partial streams or unresolved placeholder states.
3. On stale hit, serve last complete snapshot and trigger background regeneration.
4. Regeneration reuses same render budget and marker protocol for consistency.

#### B.5 SEO and Metadata Guarantees

1. Head tags (title, canonical, meta, structured data) must be in initial shell flush.
2. No critical SEO signal may depend on async boundaries.
3. Crawler profile rendering path disables client patch dependency.

#### B.6 Observability for Streaming SSR

Record and export these metrics:

- `ssr_ttfb_ms`
- `ssr_first_visible_chunk_ms`
- `ssr_full_stream_complete_ms`
- `ssr_boundary_resolve_ms{boundary}`
- `ssr_stream_cancelled_total`
- `ssr_render_budget_exceeded_total`

#### B.7 Testing Strategy

1. Unit tests
   - marker ordering, deterministic boundary ids, nested boundary semantics.
2. Integration tests
   - client disconnect mid-stream, boundary timeout fallback, retry safe patching.
3. Load tests
   - mixed fast/slow client distribution, sustained concurrency, memory pressure.
4. Regression tests
   - verify initial shell always includes SEO-critical metadata.

### Performance SLOs (Initial)

- SSR TTFB p95 < 200ms (local benchmark profile).
- First visible content p95 < 400ms.
- p99 full-stream completion within route-specific budget.
- Memory growth remains bounded under sustained concurrent render load.
- Stream cancellation and timeout behavior remains deterministic under chaos tests.

### B.8 Implementation Task List (Execution Order)

- [x] Define suspense marker protocol contract (pending/resolved/error) and parser.
- [x] Add streaming render metrics model (TTFB, first visible chunk, completion, boundary latencies).
- [x] Instrument chunk writer and server response path with metrics capture hooks.
- [x] Add cancellation-aware rendering (abort unresolved futures on disconnect).
- [x] Enforce bounded chunk buffers and per-request render budgets.
- [x] Integrate ISR rules to cache only finalized snapshots.
- [x] Build load test profile for mixed fast/slow clients.
- [x] Gate CI on streaming SLO thresholds and regression checks.

---

## 4) Workstream C — WASM Delivery and Hydration Strategy

### Goal

Keep the Rust-only frontend experience while preventing WASM payload/TTI regressions.

### Design

#### C.1 Hydration Budget System

- Define per-route hydration budget (size + startup time).
- Fail CI for budget regressions in production profile.

#### C.2 Binary Optimization Pipeline

- Enforce LTO, symbol stripping, and optimized wasm compilation pipeline.
- Include source-map policy for dev only.

#### C.3 Smart Preload Policy

- Inject preload hints only for pages with islands.
- Defer non-critical island hydration via priority classes.

#### C.4 Pragmatic Escape Hatch

- Optional minimal-JS fallback for trivial interactions where full WASM boot is wasteful.
- Must be explicit and auditable (linted, documented, measured).

### Compatibility Policy

- Graceful SSR-only behavior when WASM fails to initialize.
- Hydration diagnostics surfaced in logs and browser console with stable error codes.

### C.5 Implementation Task List (Execution Order)

- [x] Define per-route hydration budget schema (size + startup-time thresholds).
- [x] Add CI gate for WASM payload size regression in production profile.
- [x] Enforce LTO, symbol stripping, and optimized wasm-pack pipeline in build tooling.
- [x] Implement smart preload hint injection for island-bearing pages.
- [x] Add priority-class deferred hydration for non-critical islands.
- [x] Build optional minimal-JS escape hatch with lint/audit support.
- [x] Add SSR-only graceful fallback when WASM initialization fails.
- [x] Surface hydration diagnostics with stable error codes in browser console and server logs.

---

## 5) Workstream D — Full-Stack Type Safety and RPC Contracts

### Goal

Guarantee compile-time correctness from DB/domain models to server APIs to client usage.

### Design

#### D.1 Typed RPC Envelope

- Introduce versioned RPC envelope with request-id, schema-version, feature flags.
- Add compatibility mode for additive schema evolution.

#### D.2 Error Taxonomy

- Standardize typed error categories (validation/authz/not-found/conflict/internal).
- Ensure deterministic mapping to HTTP status and client-safe payloads.

#### D.3 Contract Testing

- Snapshot tests for API schemas and wire payloads.
- Backward compatibility test gate in CI for minor releases.

#### D.4 Data Layer Integration

- Align migration tooling with generated type metadata checks.
- Prevent deploy if runtime schema drift exceeds policy threshold.

### D.5 Implementation Task List (Execution Order)

- [x] Define versioned RPC envelope struct with request-id, schema-version, and feature flags.
- [x] Implement additive schema evolution compatibility mode in envelope layer.
- [x] Standardize typed error categories and deterministic HTTP status mapping.
- [x] Add snapshot tests for API schemas and wire payloads.
- [x] Add backward compatibility test gate in CI for minor releases.
- [x] Align migration tooling with generated type metadata checks.
- [x] Implement deploy-time schema drift detection and policy threshold gate.

---

## 6) Workstream E — Enterprise Security and Observability

### Goal

Provide production-grade security posture and operations baseline by default.

### Design

#### E.1 Secrets Governance

- Strict source policy by environment (dev permissive, prod locked).
- Startup-time validation with explicit failure reasons.

#### E.2 Supply Chain Controls

- Enforce license/advisory/registry policy in CI and release workflows.
- Produce machine-readable attestation artifact for each release.

#### E.3 Unified Telemetry Model

- Standard fields: trace-id, request-id, service, route, tenant, user (redacted policy).
- Built-in RED metrics (Rate, Errors, Duration) per endpoint.

#### E.4 Security Baselines

- Secure headers baseline, CSRF strategy for browser mutation routes, JWT/OIDC hardening profile.
- Add threat-model checklist to `krab_cli release check`.

### E.5 Implementation Task List (Execution Order)

- [x] Implement secrets source policy per environment (dev permissive, prod locked).
- [x] Add startup-time secrets validation with explicit failure reasons.
- [x] Enforce license/advisory/registry policy via `cargo-deny` in CI.
- [x] Produce machine-readable attestation artifact for each release.
- [x] Implement unified telemetry model (trace-id, request-id, RED metrics per endpoint).
- [x] Add secure headers baseline and CSRF strategy for browser mutation routes.
- [x] Implement JWT/OIDC hardening profile.
- [x] Build `krab_cli release check` threat-model checklist automation.

---

## 7) Workstream F — Developer Experience and Tooling

### Goal

Reach competitive day-to-day productivity with modern DX expectations.

### Design

#### F.1 Fast Dev Loop

- Distinguish asset-only change vs Rust code change.
- Asset-only updates hot-patch without full rebuild.

#### F.2 Macro Diagnostics

- Improve span mapping and actionable compile errors.
- Add compile-fail tests for common mistakes in view/island/server macros.

#### F.3 IDE Reliability

- Keep macro expansion rust-analyzer friendly.
- Publish known limitations and recommended workspace settings.

#### F.4 Starter Templates

- `krab new` templates: SaaS monolith, edge SSR app, event-stream dashboard.
- Include production-ready CI, telemetry, and deployment manifests.

### F.5 Implementation Task List (Execution Order)

- [x] Implement asset-only hot-patch dev loop (skip full Rust rebuild for CSS/static changes).
- [x] Improve macro span mapping and actionable compile error messages.
- [x] Add compile-fail tests for common view/island/server macro mistakes.
- [x] Verify macro expansion rust-analyzer compatibility and document known limitations.
- [x] Publish recommended IDE workspace settings.
- [x] Build `krab new` starter templates (SaaS monolith, edge SSR, event-stream dashboard).
- [x] Include production-ready CI, telemetry, and deployment manifests in templates.

---

## 8) Delivery Roadmap (Phased)

## Phase 0 — Stabilize Foundations (2–4 weeks)

- Freeze public macro surface for one cycle.
- Define SLO baselines and benchmark harness.
- Introduce compatibility test pipeline.

## Phase 1 — Monolith Excellence (4–8 weeks)

- Single-binary golden path.
- Streaming SSR core + ISR coherence.
- Hydration budget checks.

## Phase 2 — Controlled Distribution (6–10 weeks)

- Service contract/adapters.
- Distributed topology feature flag.
- Cross-service tracing and retry/deadline policies.

## Phase 3 — Enterprise Hardening (4–6 weeks)

- Secrets governance enforcement.
- Security checklist automation.
- Release attestations and policy gates.

## Phase 4 — Adoption Acceleration (ongoing)

- Reference apps + migration guides.
- Performance case studies vs comparable stacks.
- Ecosystem integrations (auth providers, DBs, observability vendors).

---

## 9) Ownership by Workspace Area

- `crates/framework/krab_core`: contracts, runtime invariants, telemetry fields, SSR engine primitives.
- `crates/framework/krab_macros`: server/island/view diagnostics, codegen stability, compatibility helpers.
- `crates/framework/krab_client`: hydration runtime, wasm bootstrap, diagnostics.
- `crates/framework/krab_server`: streaming transport and middleware defaults.
- `crates/tooling/krab_cli`: topology tooling, release checks, benchmark and policy commands.
- `crates/tooling/krab_orchestrator`: local multi-service orchestration, bootstrap lifecycle, health coordination.
- `services/*`: reference implementation and production profile validation.

---

## 10) Benchmark and Quality Gates

### Performance Gates

- SSR p95 and p99 latency thresholds.
- WASM payload size and startup thresholds.
- Concurrency soak test pass criteria.

### Reliability Gates

- Error budget SLO checks.
- Chaos tests for upstream dependency degradation.
- Rollback rehearsal for migrations.

### Security Gates

- No critical advisory dependencies.
- Secret policy compliance.
- Auth and session policy conformance checks.

---

## 11) Risks and Mitigations

1. **WASM startup regressions**  
   Mitigation: strict budget gates, lazy hydration, fallback strategy.

2. **Macro complexity harming DX**  
   Mitigation: simplify grammar where possible; prioritize compiler diagnostics; extensive compile-fail tests.

3. **Distributed mode operational complexity**  
   Mitigation: keep monolith default; distribution opt-in; automated topology doctor.

4. **Maintenance burden across many crates**  
   Mitigation: clear ownership boundaries, API stability policy, compatibility CI.

---

## 12) Definition of Success (12-Month Horizon)

- Monolith mode becomes default choice for new internal projects.
- At least one production deployment scales to distributed mode without major rewrite.
- Performance reports show measurable SSR/throughput advantage over baseline Node stacks.
- Developer onboarding time reduced via templates and guardrails.
- Security/operations checks become standard release gates, not optional tasks.

---

## 13) Related Documents

Detailed plans and supporting documentation referenced by this strategic plan:

### Architecture & Design

- `plans/02_architecture_design.md` — Architecture design decisions
- `plans/09_workspace_structure_standard.md` — Workspace layout standard
- `plans/api_governance.md` — API governance policies

### Protocol & Scaling (Workstream A)

- `plans/api_protocol_flexibility_plan.md` — Detailed protocol flexibility implementation
- `plans/protocol_flexibility/` — Phase-by-phase protocol flexibility plans
- `docs/protocol_flexibility.md` — Protocol flexibility overview

### Performance & SSR (Workstream B)

- `plans/benchmark_testing_plan.md` — Benchmark testing strategy
- `plans/05_performance_and_efficiency.md` — Performance and efficiency targets
- `plans/slo_alerts.md` — SLO alerting definitions
- `plans/load_test_artifacts/` — Load test evidence and reports

### Security & Operations (Workstream E)

- `docs/security.md` — Security architecture and policies
- `docs/deployment.md` — Deployment procedures
- `plans/oncall_playbook.md` — On-call runbook
- `plans/db_rollback_runbook.md` — Database rollback procedures

### Developer Experience (Workstream F)

- `plans/07_dev_workflow.md` — Developer workflow guide
- `plans/environment_template.md` — Environment configuration template

### Production Readiness

- `plans/08_production_readiness.md` — Production readiness checklist
- `plans/06_completeness_report.md` — Framework completeness report
- `plans/fullstack_remediation.md` — Full-stack remediation items
- `plans/service_dashboard.json.md` — Service monitoring dashboard config

---

## 14) Final Strategic Recommendation

Keep the six core pillars, but enforce this execution order:

1. **Monolith excellence first** (best DX + fastest adoption),
2. **SSR/streaming + hydration budgets second** (clear user-perceived wins),
3. **distribution as an opt-in evolution path**,
4. **enterprise controls baked into defaults, not bolt-ons**.

This sequence maximizes traction while preserving Krab’s long-term enterprise differentiation.
