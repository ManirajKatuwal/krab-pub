# Service Level Objectives (SLOs) and Alerting Policy

## 1. Availability SLO
- **Objective:** 99.9% of requests succeed (2xx, 3xx, 4xx) over a rolling 30-day window.
- **SLI:** `(Total requests - 5xx responses) / Total requests`
- **Alert linkage:**
	- `AvailabilityBurnRateFast` (P1): `rate(krab_response_5xx_total[5m]) / (rate(krab_requests_total[5m]) + 0.001) > 0.014` for 5m
	- `AvailabilityBurnRateSlow` (P2): `rate(krab_response_5xx_total[30m]) / (rate(krab_requests_total[30m]) + 0.001) > 0.006` for 15m

## 2. Latency SLO
- **Objective:** 99% of requests complete within 500ms.
- **SLI:** `histogram_quantile(0.99, rate(krab_request_duration_ms_bucket[5m]))`
- **Alert linkage:**
	- `LatencyBurnRateFast` (P2):
		`(
			rate(krab_request_duration_ms_count[5m]) -
			rate(krab_request_duration_ms_bucket{le="500"}[5m])
		) / (rate(krab_request_duration_ms_count[5m]) + 0.001) > 0.10` for 5m
	- `LatencyBurnRateSlow` (P2):
		`(
			rate(krab_request_duration_ms_count[30m]) -
			rate(krab_request_duration_ms_bucket{le="500"}[30m])
		) / (rate(krab_request_duration_ms_count[30m]) + 0.001) > 0.05` for 15m

## 3. Auth Budget Burn
- **Objective:** Detect credential stuffing or misconfigured clients.
- **Indicator:** `rate(krab_auth_failures_total[5m])`
- **Alert linkage:**
	- `AuthBudgetBurnFast` (P2): `rate(krab_auth_failures_total[5m]) > 10` for 5m
	- `AuthBudgetBurnSlow` (P2): `rate(krab_auth_failures_total[30m]) > 5` for 15m

## 4. Readiness SLO Degradation
- **Objective:** Ensure service dependencies are healthy.
- **Indicator:** `readiness` endpoint status.
- **Alert linkage:**
	- `ServiceNotReady` (P1): `krab_dependency_up{critical="true"} == 0` for 30s

## 5. Alert Routing
- **P1 (Critical):** PagerDuty (On-call engineer) - Immediate response required.
- **P2 (Warning):** Slack/Email (Team channel) - Response within business hours.

## 6. Runbook Mapping

All burn-rate alerts are mapped to [docs/operations/oncall_playbook.md](oncall_playbook.md) and must include the `runbook` annotation in Prometheus rules.
