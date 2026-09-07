# Production Readiness Plan

**Status:** Signed off for publication readiness; Phases 3 and 4 complete, with the Phase 1 and Phase 2 load-validation items still open (see section 5)
**Last Updated:** 2026-09-01
**Scope:** Distributed runtime state, data-layer decoupling, and operational controls required for stable production release.

---

## 1) Objective and exit condition

Krab is currently in pre-production hardening. This plan defines the remaining architecture and operations work needed to promote from beta to stable.

### Exit condition

Promotion to stable requires all of the following:

1. Mandatory CI gates are green.
2. No unresolved high/critical dependency advisories.
3. Critical production blockers are closed or formally risk-accepted.
4. Runbook + on-call + SLO evidence is available and reviewed.

See [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) for stable promotion requirements and release channel definitions.

---

## 2) Workstream A — Distributed runtime state

### Problem

In-memory counters/caches create single-instance behavior and inconsistent enforcement across replicas.

### Target

Use shared Redis-backed state for:

- rate limit windows
- auth failure tracking
- cache entries required for cross-replica consistency

### Implementation approach

- Keep backend abstraction in `krab_core` store layer.
- Ensure middleware uses shared store operations instead of local-only counters.
- Define TTL and key naming policy for each state domain.

### Completion criteria

- `N=1` and `N=3` runs show policy consistency.
- No cross-replica bypass for security controls.
- Error rate and latency stay within SLO thresholds.

---

## 3) Workstream B — Data-layer decoupling

### Problem

Users service currently relies on direct PostgreSQL-specific query usage in service logic.

### Target

Isolate persistence behind repository interfaces so runtime backend choice is configuration-driven.

### Implementation approach

- Introduce repository trait boundaries for user operations.
- Keep PostgreSQL adapter as current production implementation.
- Add startup-time driver resolution and typed unsupported-driver failure.
- Add behavior parity tests for future driver adapters.

### Completion criteria

- Service logic depends on repository interfaces, not DB-specific query primitives.
- Driver selection is explicit and validated at startup.
- Contract tests pass for each supported adapter.

---

## 4) Workstream C — Operational hardening

### Target controls

1. **Secret sourcing**
   - Support `*_FILE` pattern for mounted secrets.
   - For non-local environments, disallow insecure fallback defaults.
2. **Deployment governance**
   - Enforce migration lifecycle checks (apply/rollback/drift).
   - Require rollback rehearsal evidence before promotion.
3. **Observability and response**
   - SLO alerts wired to on-call runbook.
   - Trace/request correlation preserved across service boundaries.

### Completion criteria

- Production paths pass without local/dev security fallbacks.
- Release artifacts include rollback guidance and incident handling links.

---

## 5) Delivery phases

### Phase 1 — Shared state baseline

- [x] Finalize distributed store integration for rate limiting and auth-failure windows. Both the per-IP rate limiter (`global_rate_limit_middleware`, `krab_core/src/http.rs`) and the auth-failure window (`krab_core/src/http_auth.rs`) increment their counters through `DistributedStore::incr`; token revocation is store-backed too. Redis provides atomic `INCR`/`EXPIRE` across replicas when `KRAB_REDIS_URL` is configured; the rate limiter honours `KRAB_RATE_LIMIT_FAIL_OPEN` and auth-failure tracking fails closed on store errors.
- [ ] Validate multi-replica consistency in CI load profiles. The gate is wired — `nft.yaml` brings up the N=3 scaled compose stack (all services on one Redis) and runs `scripts/shared_state_validation.py`, which asserts a per-IP `429` boundary is reached and holds while requests spread across replicas — but it has never executed: GitHub Actions runs in this repository have never completed (a billing issue), and `nft.yaml`'s only two runs both failed. The evidence behind this item is therefore a local containerised run of the same script against the same stack, 2026-08-19: 240 samples across 24 workers, first `429` at request index 102, 118 `200` and 122 `429`, result PASS — recorded in the gitignored `benchmarks/shared_state_validation.json`, so it is not reviewable from a clone. Auth-failure-window consistency across replicas is not exercised by that script at all. Stays open until the gate runs green.

### Phase 2 — Service cache and policy hardening

- [x] Complete shared cache strategy for frontend-sensitive routes. `service_frontend/src/cache.rs` resolves a per-route `CacheAuthority` (Isr / Distributed / None) from the render policy: ISR pages go through `IsrCache::with_store(runtime.store.clone())`, so with `KRAB_REDIS_URL` set every replica reads and invalidates the same entries (`main.rs:1191`), with a cold-miss render lease and background revalidation (stale-while-revalidate); SWR/Static routes use the store-backed distributed cache (`cache:{namespace}:{uri}`, TTL via `KRAB_DISTRIBUTED_CACHE_TTL_SECS`). Discovery-key hardening: only allowlisted query parameters participate in keys (`FRONTEND_ISR_QUERY_ALLOWLIST`) and the locale is keyed separately, so attacker-chosen query strings cannot mint unbounded cache entries or poison another locale. Cache read failures degrade to rendering (fail open), never 500.
- [ ] Validate TTL and cache invalidation behavior under load. Functional TTL/invalidation is unit-tested (`isr_cache_serves_fresh_then_stale`, `isr_stale_request_triggers_background_regeneration`, `cache_authority_prefers_isr_for_page_routes_over_distributed_cache` in `service_frontend/src/main.rs`) against the in-memory store, but no NFT/load scenario exercises the distributed (Redis) cache TTL/invalidation path under concurrent replicas — the outstanding part of this item.

### Phase 3 — Repository boundary rollout

- [x] Move users-service persistence behind repository interfaces. `service_users` routes all data access through `krab_core::repository::UserRepository`; the GraphQL/REST/RPC adapters call `domain.get_me` (which holds `Arc<dyn UserRepository>`), and only the `PostgresUserRepository`/`SqliteUserRepository` implementations touch SQL. The single read query is driver-parameterised via `SqlDialect` (`$1` vs `?`).
- [x] Add startup driver selection and typed validation errors. `krab_core::db::resolve_db_driver` reads `KRAB_DB_DRIVER` (postgres default, sqlite) and returns a typed error naming the supported set; `build_user_repository` rejects a driver/pool mismatch at startup and the service boots on both drivers.
- [x] Add compatibility test matrix for future adapters. The `service_users` suite runs the contract/acceptance/parity/startup tests against both `SqliteUserRepository` (in-memory pool) and `PostgresUserRepository`, and pins the driver↔pool mismatch and unsupported-driver error paths.

### Phase 4 — Release-readiness controls

- [x] Complete secret injection policy (`*_FILE` and vault-ready precedence).
- [x] Publish evidence bundle (CI gates, NFT trends, rollback rehearsal).
- [x] Final sign-off with explicit residual risk acceptance.

---

## 6) Evidence required for publication

Before production publication, include:

1. CI evidence for required workflows.
2. Latest load/NFT summary + trend history.
3. Blocker closure or signed risk acceptance log.
4. Release notes + rollback notes.

Supporting references:

- [`RELEASE_POLICY.md`](../../RELEASE_POLICY.md)
- [`docs/operations/oncall_playbook.md`](oncall_playbook.md)
- [`docs/operations/slo_alerts.md`](slo_alerts.md)
- `benchmarks/latest_summary.md` (generated by the NFT workflow)
- Latest certification index generated by `krab release certify`: `internal/audit/release-certify/latest.md` (maintainer-local, gitignored)

---

## 7) Publication sign-off

### Completion summary

- Required CI and audit evidence recorded in `internal/reports/PRE_RELEASE_AUDIT_REPORT.md` (maintainer-local, gitignored).
- Latest certification bundle indexed in `internal/audit/release-certify/latest.md` (maintainer-local, gitignored).
- Rollback evidence recorded in `internal/audit/evidence/rollback-rehearsal-evidence.txt` (maintainer-local, gitignored).
- Operational response mapping recorded in [`docs/operations/oncall_playbook.md`](oncall_playbook.md).
- Release communication reviewed in [`CHANGELOG.md`](../../CHANGELOG.md).

### Author sign-off

- **Approved by:** Maniraj Katuwal
- **Role:** Author
- **Decision:** Ready for publication
- **Timestamp (UTC):** 2026-06-04T18:01:26Z
- **Residual risk acceptance:** No open Critical findings and no open High findings without explicit waiver.
