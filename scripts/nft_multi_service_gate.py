import json
import math
import os
import pathlib
import statistics
import sys
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
THRESHOLDS_PATH = ROOT / "plans" / "load_test_artifacts" / "thresholds.json"
RESULT_PATH = ROOT / "plans" / "load_test_artifacts" / "latest_summary.md"


def env(name, default):
    value = os.getenv(name)
    if value is None or not value.strip():
        return default
    return value.strip()


FRONTEND_BASE_URL = env("KRAB_FRONTEND_BASE_URL", "http://127.0.0.1:3000")
AUTH_BASE_URL = env("KRAB_AUTH_BASE_URL", "http://127.0.0.1:3001")
USERS_BASE_URL = env("KRAB_USERS_BASE_URL", "http://127.0.0.1:3002")
BEARER_TOKEN = env("KRAB_BEARER_TOKEN", "test-token")


def percentile(values, pct):
    if not values:
        return 0
    sorted_values = sorted(values)
    index = int(math.ceil((pct / 100.0) * len(sorted_values))) - 1
    index = max(0, min(index, len(sorted_values) - 1))
    return sorted_values[index]


def run_get(url, headers=None):
    req = urllib.request.Request(url=url, method="GET", headers=headers or {})
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            resp.read()
            status = resp.status
    except urllib.error.HTTPError as err:
        status = err.code
    except Exception:
        status = 0
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    return status, elapsed_ms


def run_post(url, body, headers=None):
    encoded = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url=url, method="POST", data=encoded, headers=headers or {})
    start = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            resp.read()
            status = resp.status
    except urllib.error.HTTPError as err:
        status = err.code
    except Exception:
        status = 0
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    return status, elapsed_ms


def benchmark_headers(sample_index, base=None):
    """
    Build per-request headers for synthetic benchmarks.

    Why: in containerized runs without explicit client IP forwarding, every request can
    collapse into the same "unknown" limiter bucket, which causes synthetic 429 spikes
    unrelated to the service latency objectives this gate validates.
    """
    headers = dict(base or {})
    octet3 = (sample_index // 254) % 254
    octet4 = (sample_index % 254) + 1
    headers["X-Forwarded-For"] = f"10.240.{octet3}.{octet4}"
    return headers


def evaluate_service(service_name, config):
    profile = config["profiles"]["load"]
    samples = int(profile["samples"])
    p95_threshold = int(profile["hard_threshold_ms"]["p95"])
    p99_threshold = int(profile["hard_threshold_ms"]["p99"])

    latencies = []
    success = 0

    if service_name == "service_frontend":
        for idx in range(samples):
            status, latency = run_get(
                f"{FRONTEND_BASE_URL}/health",
                headers=benchmark_headers(idx),
            )
            latencies.append(latency)
            if 200 <= status < 300:
                success += 1
    elif service_name == "service_auth":
        base_headers = {"Authorization": f"Bearer {BEARER_TOKEN}"}
        for idx in range(samples):
            status, latency = run_get(
                f"{AUTH_BASE_URL}/api/v1/private",
                headers=benchmark_headers(idx, base=base_headers),
            )
            latencies.append(latency)
            if 200 <= status < 300:
                success += 1
    elif service_name == "service_users":
        base_headers = {
            "Authorization": f"Bearer {BEARER_TOKEN}",
            "Content-Type": "application/json",
        }
        payload = {"query": "{ __typename }"}
        for idx in range(samples):
            status, latency = run_post(
                f"{USERS_BASE_URL}/api/v1/graphql",
                payload,
                headers=benchmark_headers(idx, base=base_headers),
            )
            latencies.append(latency)
            if 200 <= status < 300:
                success += 1
    else:
        raise ValueError(f"Unsupported service: {service_name}")

    p95 = round(percentile(latencies, 95))
    p99 = round(percentile(latencies, 99))
    error_rate_percent = round(((samples - success) / max(samples, 1)) * 100.0, 3)

    passed = (
        p95 <= p95_threshold
        and p99 <= p99_threshold
        and error_rate_percent <= 0.5
    )

    return {
        "service": service_name,
        "samples": samples,
        "p95_ms": p95,
        "p99_ms": p99,
        "threshold_p95_ms": p95_threshold,
        "threshold_p99_ms": p99_threshold,
        "error_rate_percent": error_rate_percent,
        "result": "PASS" if passed else "FAIL",
        "mean_ms": round(statistics.mean(latencies), 3) if latencies else 0,
    }


def parse_args():
    mode = "single"
    markdown_out = RESULT_PATH
    json_out = None

    args = sys.argv[1:]
    index = 0
    while index < len(args):
        arg = args[index]
        if arg == "--mode" and index + 1 < len(args):
            mode = args[index + 1]
            index += 2
            continue
        if arg == "--markdown-out" and index + 1 < len(args):
            markdown_out = pathlib.Path(args[index + 1])
            index += 2
            continue
        if arg == "--json-out" and index + 1 < len(args):
            json_out = pathlib.Path(args[index + 1])
            index += 2
            continue
        raise SystemExit(f"unknown argument: {arg}")

    return mode, markdown_out, json_out


def main():
    mode, markdown_out, json_out = parse_args()
    thresholds = json.loads(THRESHOLDS_PATH.read_text(encoding="utf-8"))
    services = thresholds["services"]

    results = []
    for service_name, config in services.items():
        results.append(evaluate_service(service_name, config))

    all_passed = all(r["result"] == "PASS" for r in results)
    timestamp = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())

    payload = {
        "timestamp_utc": timestamp,
        "mode": mode,
        "scope": ["service_frontend", "service_auth", "service_users"],
        "results": results,
        "gate": "PASS" if all_passed else "FAIL",
    }

    lines = []
    lines.append("# Latest Load Test Summary")
    lines.append("")
    lines.append(f"- **Timestamp (UTC):** {timestamp}")
    lines.append("- **Scope:** service_frontend + service_auth + service_users")
    lines.append(f"- **Mode:** {mode} load profile against SLO thresholds")
    lines.append("")
    lines.append("| Service | Samples | p95 (ms) | p99 (ms) | Threshold p95/p99 (ms) | Error Rate (%) | Result |")
    lines.append("|---|---:|---:|---:|---:|---:|---|")

    for item in results:
        lines.append(
            f"| {item['service']} | {item['samples']} | {item['p95_ms']} | {item['p99_ms']} | "
            f"{item['threshold_p95_ms']} / {item['threshold_p99_ms']} | {item['error_rate_percent']} | {item['result']} |"
        )

    lines.append("")
    lines.append(f"- Release gate decision: **{'PASS' if all_passed else 'FAIL'}**")

    markdown_out.write_text("\n".join(lines) + "\n", encoding="utf-8")

    if json_out is not None:
        json_out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    if not all_passed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
