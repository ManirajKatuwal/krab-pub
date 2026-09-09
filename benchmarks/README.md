# Benchmarks and Load-Test Artifacts

This directory is the canonical repository for non-functional multi-service load-test evidence and regression controls. `thresholds.json` and `benchmark_config.json` are **CI inputs** and must stay tracked.

## Tracked vs generated

| Tracked (committed) | Generated (gitignored) |
|---|---|
| `thresholds.json` | `latest_summary.md`, `latest_summary_single.md`, `latest_summary_scaled.md` |
| `benchmark_config.json` | `single_replica_results.json`, `scaled_replica_results.json` |
| `trend_history.csv` | `shared_state_validation.json` |
| `external_results.json`, `external_summary.md` | `shared_state_auth_failure_validation.json` |
| `targeted_hardening_results.json` | `release_evidence_bundle.json`, `release_evidence_bundle.md` |

The generated patterns are listed in [`.gitignore`](../.gitignore). CI uploads
them as workflow artifacts rather than committing them.

## Purpose

- Preserve trend history for frontend, auth, and users service non-functional tests.
- Define per-service hard p95/p99 SLO thresholds that can fail CI automatically.
- Validate horizontal scaling behavior (1 replica vs multi-replica) with shared state enabled.
- Keep reproducible, date-stamped artifacts for audits and release readiness.

## Layout

- `thresholds.json` — source of truth for per-service percentile limits, horizontal-scaling gates, and auto-fail rules.
- `trend_history.csv` — append-only time series used for regression detection.
- `latest_summary.md` — most recent run summary with service-by-service p95/p99 outcomes and gate decision. Generated at runtime; absent until a run happens.
- `latest_summary_single.md` — single-replica (`N=1`) service results. Generated at runtime; absent until a run happens.
- `latest_summary_scaled.md` — scaled (`N=3`) service results. Generated at runtime; absent until a run happens.
- `shared_state_validation.json` — distributed/shared-state abuse-control validation output (expects block behavior under scaled topology). Generated at runtime; absent until a run happens.
- `shared_state_auth_failure_validation.json` — auth-failure limiter validation output: replays invalid-token requests against the scaled stack and asserts the first `429` lands at or below `KRAB_SHARED_STATE_MAX_BLOCK_INDEX`, which is what proves the per-IP auth-failure limiter produced the block rather than the global rate limiter (whose token bucket would block far later, near `KRAB_RATE_LIMIT_CAPACITY`). Runs under its own `KRAB_SHARED_STATE_CLIENT_IP` so the token bucket it meets is full — a bucket drained by an earlier scenario blocks early enough to satisfy the bound on its own. Generated at runtime; absent until a run happens.
- `release_evidence_bundle.json` / `release_evidence_bundle.md` — consolidated release-evidence presence report aligned to release policy artifacts. Generated at runtime; absent until a run happens.

## CI/Workflow Contract

1. Non-functional tests execute for `service_frontend`, `service_auth`, and `service_users` profiles.
2. CI parses test outputs, computes p95/p99 metrics, and evaluates per-service thresholds in `thresholds.json`.
3. CI executes scaling validation with `N=1` and `N=3` replicas using shared state (`KRAB_REDIS_URL`) via internal-network runner topology (`docker-compose.nft.yaml`).
4. CI appends one new row to `trend_history.csv` per service/profile/replica mode.
5. CI fails the run if:
   - any hard threshold is exceeded, or
   - regression percentage exceeds allowed drift relative to the selected baseline window, or
   - horizontal-scaling regression exceeds allowed drift, or
   - shared-state-required validation runs without shared state enabled.

Additional governance automation:

- `scripts/shared_state_validation.py` verifies scaled-stack shared-state blocking behavior and publishes `shared_state_validation.json`. Run with `KRAB_SHARED_STATE_SCENARIO=auth_failure` (and `KRAB_SHARED_STATE_OUT`) it validates the auth-failure limiter instead and publishes `shared_state_auth_failure_validation.json`.
- `scripts/release_evidence_bundle.py` generates a release evidence manifest for CI artifact upload and audit traceability.

## Artifact Retention

- Keep at least the last 90 days of trend history in Git.
- Never rewrite existing trend rows; only append corrections with explicit `notes`.
- Include release tag/commit in each row for traceability.
