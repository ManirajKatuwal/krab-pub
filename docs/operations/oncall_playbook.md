# On-Call Playbook

## Alert-to-Runbook Mapping

| Alert | Priority | Runbook Section |
|---|---|---|
| `AvailabilityBurnRateFast` | P1 | `P1: Availability Burn Rate` |
| `AvailabilityBurnRateSlow` | P2 | `P2: Availability Burn Rate (Slow)` |
| `LatencyBurnRateFast` | P2 | `P2: Latency Burn Rate` |
| `LatencyBurnRateSlow` | P2 | `P2: Latency Burn Rate` |
| `AuthBudgetBurnFast` | P2 | `P2: Auth Budget Burn` |
| `AuthBudgetBurnSlow` | P2 | `P2: Auth Budget Burn` |

## P1: Availability Burn Rate

### Symptoms
- `AvailabilityBurnRateFast` alert firing for >= 5 minutes.
- 5xx ratio sustained above the SLO burn threshold.

### Investigation Steps
1. Check impacted service/route panels for concentrated 5xx paths.
2. Correlate with deployment timeline and rollback immediately if regression aligns with deploy.
3. Verify dependency readiness (`/ready`) and database/auth dependency health.
4. Inspect logs for panic signatures, timeout cascades, or pool exhaustion.
5. Verify Prometheus inputs are healthy for canonical series:
   - `krab_http_responses_total{class="5xx"}`
   - `krab_http_requests_by_protocol_total`
   - Compatibility aliases may exist for legacy dashboards: `krab_response_5xx_total`

### Immediate Actions
- If deploy related: rollback first, investigate second.
- If dependency outage: fail over or isolate traffic from failing dependency path.
- If saturation: scale horizontally and reduce non-critical workload.

## P2: Availability Burn Rate (Slow)

### Symptoms
- `AvailabilityBurnRateSlow` alert firing for >= 15 minutes.

### Investigation Steps
1. Identify chronic failure route(s) and top contributing status codes.
2. Confirm error budget trajectory and expected exhaustion window.
3. Open remediation issue with owner and ETA if no immediate rollback path exists.

### Actions
- Schedule controlled mitigation (config fix, circuit breaker tuning, query/index remediation).
- Keep alert muted only with explicit incident ticket and owner acknowledgement.

## P1: Availability SLO Degradation

### Symptoms
- 5xx error rate > 1% on dashboard.
- `HighErrorRate` and/or `AvailabilityBurnRateFast` alert firing.

### Investigation Steps
1. **Check Dashboard:** Is the error spike correlated with a specific route?
2. **Check Logs:** Filter for `status=500` in logs. Look for panic stacks or DB connection errors.
3. **Check Dependencies:** Is the database or auth provider down? (Check `Dependency Health` panel).
4. **Check Deployments:** Was there a recent deploy? If so, rollback immediately.

### Resolution
- **Database Down:** Check cloud provider status. Failover if necessary.
- **Bad Deploy:** Execute rollback via CI or CLI.
- **Traffic Spike:** Scale up replicas if CPU/Memory is saturated.

## P1: Readiness Degradation

### Symptoms
- `ServiceNotReady` alert firing.
- Load balancer removing healthy pods.

### Investigation Steps
1. **Check `/ready` Endpoint:** `curl -v localhost:PORT/ready`.
2. **Identify Failing Dependency:** Which dependency in the JSON response is `ready: false`?
3. **Check Logs:** Search for "readiness check failed" or similar warnings.

## P2: Latency SLO Degradation

### Symptoms
- `HighP99Latency` and/or `LatencyBurnRateFast` alert firing.
- Slow page loads reported by users.

### Investigation Steps
1. **Trace Request:** Grab a `trace_id` from slow logs and analyze span duration.
2. **Database:** Check for slow queries or missing indexes.
3. **Resources:** Check for CPU throttling or memory leaks (GC pauses).

## P2: Latency Burn Rate

### Symptoms
- `LatencyBurnRateFast` or `LatencyBurnRateSlow` alert firing.
- Growing fraction of requests above 500ms budget.

### Investigation Steps
1. Compare p95/p99 across routes to identify hotspot handlers.
2. Check DB query latency and connection pool pressure.
3. Validate cache hit ratios and upstream dependency latency.
4. Validate histogram contract and scrape freshness for:
   - `krab_http_request_duration_seconds_bucket`
   - `krab_http_request_duration_seconds_count`
   - Compatibility aliases may exist for legacy dashboards: `krab_request_duration_ms_*`

### Actions
- Reduce expensive query paths (indexes, batching, cache).
- Increase service replica count if saturation is confirmed.
- Apply targeted rate-limit tightening on expensive endpoints where appropriate.

## P2: Auth Budget Burn

### Symptoms
- `AuthFailureBurst` and/or `AuthBudgetBurnFast` alert firing.

### Investigation Steps
1. **Identify Source IP:** Check logs for `auth_failure` events and group by client IP.
2. **Block Malicious Traffic:** If attack, block IP at WAF/Load Balancer level.
3. **Check Configuration:** Ensure `KRAB_OIDC_ISSUER` and keys are correctly configured.

### Additional Actions for Burn-Rate Alerts
4. If `AuthBudgetBurnFast` persists, enable temporary protective controls (stricter rate limits, WAF rule).
5. If `AuthBudgetBurnSlow` persists, create follow-up for client misconfiguration or abusive traffic source remediation.

### Hardening Controls to Verify During Incident

- **Rate-limit store failure policy** (`KRAB_RATE_LIMIT_FAIL_OPEN`)
  - `true`: requests continue on store outage (availability priority)
  - `false`: requests are rejected with `429` on store outage (abuse-resistance priority)
- **Proxy header trust** (`KRAB_TRUST_PROXY_HEADERS`)
  - `false` (default): ignores `x-forwarded-for` and `x-real-ip`
  - `true`: forwarded IP headers are trusted for per-IP controls
- **JWT algorithm policy** (`KRAB_JWT_ALLOWED_ALGS`)
  - Ensure configured allowlist matches provider signing algorithms.
