import json
import os
import pathlib
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Optional, Tuple


ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "plans" / "load_test_artifacts" / "shared_state_validation.json"


def env(name: str, default: str) -> str:
    value = os.getenv(name)
    if value is None:
        return default
    stripped = value.strip()
    return stripped if stripped else default


def auth_base() -> str:
    return env("KRAB_AUTH_BASE_URL", "http://127.0.0.1:3001").rstrip("/")


def build_request(url: str, token: str):
    return urllib.request.Request(
        url=url,
        method="GET",
        headers={"Authorization": f"Bearer {token}"},
    )


def hit_url(url: str, headers: Optional[dict] = None, timeout: float = 5.0) -> Tuple[int, float]:
    req = urllib.request.Request(url=url, method="GET", headers=headers or {})
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            resp.read()
            status = resp.status
    except urllib.error.HTTPError as err:
        status = err.code
    except Exception:
        status = 0
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    return status, round(elapsed_ms, 3)


def hit_private(url: str, token: str):
    return hit_url(url, headers={"Authorization": f"Bearer {token}"})


def precheck_target(base_url: str, private_url: str, token: str):
    health_status, health_latency = hit_url(f"{base_url}/health")
    private_status, private_latency = hit_private(private_url, token)
    precheck = {
        "health_status": health_status,
        "health_latency_ms": health_latency,
        "private_status": private_status,
        "private_latency_ms": private_latency,
    }

    if health_status == 0 and private_status == 0:
        return False, "target_unreachable", precheck

    return True, None, precheck


def run_shared_state_check():
    token = env("KRAB_BEARER_TOKEN", "test-token")
    samples = int(env("KRAB_SHARED_STATE_SAMPLES", "240"))
    workers = int(env("KRAB_SHARED_STATE_WORKERS", "24"))
    threshold_status = int(env("KRAB_SHARED_STATE_EXPECT_STATUS", "429"))
    base_url = auth_base()
    target_url = f"{base_url}/api/v1/private"

    precheck_ok, precheck_reason, precheck = precheck_target(base_url, target_url, token)

    if not precheck_ok:
        result = {
            "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "scenario": "shared_state_rate_limit_validation",
            "target": target_url,
            "samples": samples,
            "workers": workers,
            "expected_block_status": threshold_status,
            "precheck": precheck,
            "result": "FAIL",
            "failure_reason": precheck_reason,
            "recommended_action": "start service_auth and verify /health responds before rerunning this script",
        }
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {OUT}")
        raise SystemExit(1)

    statuses = []
    latencies = []

    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futures = {
            pool.submit(hit_private, target_url, token): idx for idx in range(samples)
        }
        ordered = [None] * samples

        for fut in as_completed(futures):
            idx = futures[fut]
            status, latency = fut.result()
            ordered[idx] = (status, latency)

    for status, latency in ordered:
        statuses.append(status)
        latencies.append(latency)

    threshold_index = None
    for idx, status in enumerate(statuses, start=1):
        if status == threshold_status:
            threshold_index = idx
            break

    status_counts = {}
    for s in statuses:
        key = str(s)
        status_counts[key] = status_counts.get(key, 0) + 1

    blocked_after_threshold = False
    if threshold_index is not None:
        blocked_after_threshold = any(s == threshold_status for s in statuses[threshold_index:])

    passed = threshold_index is not None and blocked_after_threshold

    result = {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "scenario": "shared_state_rate_limit_validation",
        "target": target_url,
        "samples": samples,
        "workers": workers,
        "expected_block_status": threshold_status,
        "precheck": precheck,
        "first_block_request_index": threshold_index,
        "blocked_after_threshold": blocked_after_threshold,
        "status_counts": status_counts,
        "mean_latency_ms": round(sum(latencies) / max(len(latencies), 1), 3),
        "result": "PASS" if passed else "FAIL",
    }

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {OUT}")

    if not passed:
        raise SystemExit(1)


if __name__ == "__main__":
    run_shared_state_check()

