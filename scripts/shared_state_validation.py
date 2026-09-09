import json
import os
import pathlib
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Optional, Tuple


ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "benchmarks" / "shared_state_validation.json"


def env(name: str, default: str) -> str:
    value = os.getenv(name)
    if value is None:
        return default
    stripped = value.strip()
    return stripped if stripped else default


def auth_base() -> str:
    return env("KRAB_AUTH_BASE_URL", "http://127.0.0.1:3001").rstrip("/")


def optional_int(name: str) -> Optional[int]:
    """Read an optional integer knob. Unset or blank means no bound.

    A malformed value is a hard error, not a silently dropped bound. This knob
    is what makes the auth_failure scenario discriminating, so a typo must fail
    the run rather than quietly downgrade it to the weaker check it exists to
    replace.
    """
    raw = os.getenv(name)
    if raw is None or not raw.strip():
        return None
    try:
        return int(raw.strip())
    except ValueError:
        raise SystemExit(f"{name}={raw!r} is not an integer")


def client_ip_override() -> Optional[str]:
    """The client IP this scenario should present, via `X-Forwarded-For`.

    Both the global token bucket and the auth-failure limiter key on the client
    IP, and every scenario in a job runs from the same container -- so the same
    IP -- against a stack that shares those counters through Redis. A scenario
    that runs second therefore inherits whatever the first one spent, and a
    partly drained token bucket blocks *earlier* than its capacity, which is
    exactly the shape `KRAB_SHARED_STATE_MAX_BLOCK_INDEX` uses to identify the
    auth-failure limiter. Giving each scenario its own IP starts it against
    full counters, so an early block can only have come from the limiter under
    test.

    Only honoured when the target trusts forwarded headers
    (`KRAB_TRUST_PROXY_HEADERS=true`, which the NFT overlay sets). Elsewhere
    the header is ignored and the socket address is used, which is what this
    script did before.
    """
    raw = os.getenv("KRAB_SHARED_STATE_CLIENT_IP")
    if raw is None or not raw.strip():
        return None
    return raw.strip()


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


def hit_private(url: str, token: str, client_ip: Optional[str] = None):
    headers = {"Authorization": f"Bearer {token}"}
    if client_ip:
        # Both limiters key on the client IP. Presenting one the scenario owns
        # keeps it off the counters an earlier scenario in the same job already
        # moved -- see `client_ip_override` for why that matters.
        headers["X-Forwarded-For"] = client_ip
    return hit_url(url, headers=headers)


def precheck_target(
    base_url: str, private_url: str, token: str, client_ip: Optional[str] = None
):
    health_status, health_latency = hit_url(f"{base_url}/health")
    private_status, private_latency = hit_private(private_url, token, client_ip)
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
    scenario_mode = env("KRAB_SHARED_STATE_SCENARIO", "rate_limit").lower()
    scenario_name = (
        "shared_state_auth_failure_validation"
        if scenario_mode == "auth_failure"
        else "shared_state_rate_limit_validation"
    )
    out_file = pathlib.Path(env("KRAB_SHARED_STATE_OUT", str(OUT)))

    if scenario_mode == "auth_failure":
        token = env("KRAB_INVALID_BEARER_TOKEN", "invalid-token-auth-failure-test")
    else:
        token = env("KRAB_BEARER_TOKEN", "test-token")

    samples = int(env("KRAB_SHARED_STATE_SAMPLES", "240"))
    workers = int(env("KRAB_SHARED_STATE_WORKERS", "24"))
    threshold_status = int(env("KRAB_SHARED_STATE_EXPECT_STATUS", "429"))
    # Optional upper bound on where the first block may appear. It is what makes
    # the auth_failure scenario discriminating: without it a block produced by
    # the global per-IP token bucket (KRAB_RATE_LIMIT_CAPACITY, default 120) is
    # indistinguishable from one produced by the auth-failure limiter. Unset
    # leaves the pass condition exactly as it was.
    max_block_index = optional_int("KRAB_SHARED_STATE_MAX_BLOCK_INDEX")
    client_ip = client_ip_override()
    base_url = auth_base()
    target_url = f"{base_url}/api/v1/private"

    precheck_ok, precheck_reason, precheck = precheck_target(
        base_url, target_url, token, client_ip
    )

    if not precheck_ok:
        result = {
            "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "scenario": scenario_name,
            "target": target_url,
            "samples": samples,
            "workers": workers,
            "expected_block_status": threshold_status,
            "max_block_index": max_block_index,
            "client_ip": client_ip,
            "precheck": precheck,
            "result": "FAIL",
            "failure_reason": precheck_reason,
            "recommended_action": "start service_auth and verify /health responds before rerunning this script",
        }
        out_file.parent.mkdir(parents=True, exist_ok=True)
        out_file.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {out_file}")
        raise SystemExit(1)

    statuses = []
    latencies = []

    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futures = {
            pool.submit(hit_private, target_url, token, client_ip): idx
            for idx in range(samples)
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

    within_max_block_index = True
    if max_block_index is not None:
        within_max_block_index = (
            threshold_index is not None and threshold_index <= max_block_index
        )

    passed = (
        threshold_index is not None
        and blocked_after_threshold
        and within_max_block_index
    )

    result = {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "scenario": scenario_name,
        "target": target_url,
        "samples": samples,
        "workers": workers,
        "expected_block_status": threshold_status,
        "max_block_index": max_block_index,
        "client_ip": client_ip,
        "precheck": precheck,
        "first_block_request_index": threshold_index,
        "blocked_after_threshold": blocked_after_threshold,
        "within_max_block_index": within_max_block_index,
        "status_counts": status_counts,
        "mean_latency_ms": round(sum(latencies) / max(len(latencies), 1), 3),
        "result": "PASS" if passed else "FAIL",
    }

    if not within_max_block_index and threshold_index is not None:
        result["failure_reason"] = (
            f"first_block_request_index={threshold_index} is above "
            f"KRAB_SHARED_STATE_MAX_BLOCK_INDEX={max_block_index}; the block did "
            "not come from the limiter under test"
        )
        result["recommended_action"] = (
            "confirm the limiter under test is configured and reachable; a first "
            "block near the global rate-limit capacity means the token bucket, "
            "not this limiter, produced the 429"
        )

    out_file.parent.mkdir(parents=True, exist_ok=True)
    out_file.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out_file}")

    if not passed:
        raise SystemExit(1)


if __name__ == "__main__":
    run_shared_state_check()

