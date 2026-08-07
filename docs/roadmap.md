# Krab Framework — Phase 0 Roadmap and Risk Log (Week 0)

Source alignment:

- [docs/operations/production_readiness.md](operations/production_readiness.md)
- [RELEASE_POLICY.md](../RELEASE_POLICY.md)

## 1) Phase objective

Establish governance and release readiness controls before deeper hardening work begins:

1. production-readiness gates
2. owner/accountability matrix
3. tracked epic breakdown with acceptance criteria and dependencies
4. frozen architecture boundary
5. release cadence and risk review loop

## 2) Frozen architecture boundary (authoritative)

Frozen architecture boundary:

- API services (`service_auth`, `service_users`) remain on Axum + shared transport/middleware in [crates/framework/krab_core/src/http.rs](../crates/framework/krab_core/src/http.rs).
- Frontend SSR/islands runtime remains on [crates/framework/krab_server/src/lib.rs](../crates/framework/krab_server/src/lib.rs).
- Shared DB lifecycle primitives remain centralized in [crates/framework/krab_core/src/db.rs](../crates/framework/krab_core/src/db.rs).

This boundary is treated as **non-negotiable for phases 1–4**, except via ADR + owner signoff.

## 3) Owner matrix (RACI-lite)

| Workstream | Accountable | Responsible | Consulted | Informed |
|---|---|---|---|---|
| Security/Auth hardening | Security Lead | `service_auth` + `service_users` owners | Platform, SRE | QA |
| DB lifecycle governance | Platform Lead | `krab_core` + `service_users` owners | SRE | QA, Security |
| API contracts/error model | Platform Lead | `krab_core` + API service owners | Frontend owner | QA |
| Observability/SLO | SRE Lead | Platform + service owners | Security | QA |
| Frontend SSR/islands resilience | Frontend Lead | `service_frontend` owner | Platform | QA |
| Integration/E2E + non-functional tests | QA Lead | QA + service owners | SRE | Security |
| DX/orchestrator hardening | Platform Lead | `krab_cli` + `krab_orchestrator` owners | SRE | All service owners |

## 4) Production-readiness gates

Each gate must be green before production certification rollout.

### Gate G1 — Security and auth

- JWT/OIDC-first mode available and validated in shared middleware in [crates/framework/krab_core/src/http.rs](../crates/framework/krab_core/src/http.rs).
- Strict issuer/audience checks, deny-by-default protected paths, and admin policy enforced for [services/service_users/src/main.rs](../services/service_users/src/main.rs).
- Negative-path auth tests pass (expired token, wrong issuer/audience, missing role/scope, revoked key).

### Gate G2 — DB lifecycle

- Promotion policy and drift checks enforced via [crates/framework/krab_core/src/db.rs](../crates/framework/krab_core/src/db.rs).
- CI validates migrate-from-zero + rollback simulation + drift failure path.
- Destructive migration guardrails and rollback runbook in place.

### Gate G3 — Contract compatibility

- Shared typed error envelope standardized in [crates/framework/krab_core/src/http.rs](../crates/framework/krab_core/src/http.rs).
- Versioning/deprecation policy documented for REST/GraphQL interfaces.
- Backward compatibility tests blocking incompatible changes in CI.

### Gate G4 — Observability/SLO

- RED/USE metrics taxonomy emitted across services.
- Dashboards + alerts for critical paths (availability, latency, auth failures, readiness SLO degradation).
- Request/trace correlation enforced end-to-end.

### Gate G5 — Frontend resilience

- SSR/islands contract checks hardened in [services/service_frontend/src/main.rs](../services/service_frontend/src/main.rs).
- Timeout/retry/fallback behavior verified under partial API degradation.
- Hydration mismatch diagnostics available and tested.

### Gate G6 — Delivery quality

- Cross-service integration and browser E2E suites stable in CI.
- Fault-injection scenarios (DB/auth/dependency degradation) exercised.
- Release checklist, rollback path, and on-call action mapping documented.

## 5) Epic breakdown (tracked execution plan)

Convert each item into issues in project tracking with dependency links and explicit acceptance criteria.

| Epic ID | Epic | Depends on | Acceptance criteria (minimum) |
|---|---|---|---|
| E0 | Governance + gate definitions | none | Owner matrix approved; gates baseline accepted by Security/Platform/QA/SRE |
| E1 | Security/auth hardening | E0 | Gate G1 green in CI + startup/runtime checks |
| E2 | DB lifecycle maturity | E0 | Gate G2 green with drift and rollback validation |
| E3 | API contract/error standardization | E0 | Gate G3 green; incompatible changes blocked |
| E4 | Observability/SLO readiness | E1, E2, E3 | Gate G4 green; actionable alerts validated |
| E5 | Frontend SSR/islands productionization | E3, E4 | Gate G5 green in degraded dependency scenarios |
| E6 | Cross-service and browser E2E depth | E1, E2, E3, E5 | Gate G6 green with fault injection |
| E7 | DX and operations hardening | E4, E6 | deterministic local/dev/prod runbooks and safe deploy gates |
| E8 | Production certification | E1..E7 | staged rollout and residual risk signoff complete |

## 6) Release cadence and ceremonies

- **Weekly hardening release** (end of each week): merged improvements behind required gates.
- **Biweekly stabilization review** (every 2 weeks): cross-functional review of regressions, risk movement, and gate quality.
- **Daily async triage**: owner updates on blockers, drift, and incidents against planned epics.

## 7) Risk log (Phase 0 initial)

| Risk ID | Risk | Likelihood | Impact | Owner | Mitigation | Trigger |
|---|---|---|---|---|---|---|
| R-001 | Auth policy drift across services | Medium | High | Security Lead | Centralize policy in [crates/framework/krab_core/src/http.rs](../crates/framework/krab_core/src/http.rs); CI negative tests | Auth test failure or inconsistent route behavior |
| R-002 | Migration rollback gaps | Medium | High | Platform Lead | Enforce rollback simulation and drift checks in CI | Failed rollback or schema mismatch |
| R-003 | Contract breakage between frontend/API | Medium | High | Platform Lead | Version policy + compatibility tests + typed error model | Frontend contract test regression |
| R-004 | Incomplete SLO alerting | Medium | Medium | SRE Lead | Dashboard/alert baseline required before promotion | Incidents without actionable alert |
| R-005 | Hydration failure under partial outages | Medium | Medium | Frontend Lead | Fallback UX path + timeout/retry behavior tests | E2E/hydration test failures |
| R-006 | Ownership ambiguity delaying approvals | High | Medium | Program owner | RACI-lite signoff + explicit gate approvers | Missed weekly release window |

## 8) Phase 0 exit criteria

Phase 0 is complete when:

1. this roadmap is accepted as the baseline governance document,
2. all epics (E0–E8) are created in tracking with dependencies + acceptance criteria,
3. architecture boundary remains fixed as documented above,
4. cadence ceremonies are scheduled and owners are assigned,
5. initial risk log is active with named owners and review frequency.
