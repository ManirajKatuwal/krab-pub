import json
import pathlib
import sys
import os


ROOT = pathlib.Path(__file__).resolve().parents[1]
THRESHOLDS_PATH = ROOT / "plans" / "load_test_artifacts" / "thresholds.json"
SUMMARY_PATH = ROOT / "plans" / "load_test_artifacts" / "latest_summary.md"


def load_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def env_float(name: str, default: float) -> float:
    raw = os.getenv(name)
    if raw is None:
        return default
    try:
        value = float(raw.strip())
        if value <= 0:
            return default
        return value
    except Exception:
        return default


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: nft_scaling_compare.py <single.json> <scaled.json>")

    single = load_json(pathlib.Path(sys.argv[1]))
    scaled = load_json(pathlib.Path(sys.argv[2]))
    thresholds = load_json(THRESHOLDS_PATH)

    limits = thresholds["horizontal_scaling"]["max_percent_increase_from_single_replica"]
    p95_limit = float(limits["p95"])
    p99_limit = float(limits["p99"])
    baseline_floor_ms = env_float("KRAB_SCALING_BASELINE_FLOOR_MS", 10.0)

    single_by_service = {item["service"]: item for item in single["results"]}
    scaled_by_service = {item["service"]: item for item in scaled["results"]}

    services = sorted(single_by_service.keys())
    all_passed = True

    lines = []
    lines.append("# Latest Load Test Summary")
    lines.append("")
    lines.append(f"- **Timestamp (UTC):** {scaled.get('timestamp_utc', '')}")
    lines.append("- **Scope:** service_frontend + service_auth + service_users")
    lines.append("- **Mode:** horizontal scaling validation (`N=1` vs `N=3`) with shared state")
    lines.append(f"- **Regression baseline floor:** {baseline_floor_ms:.1f} ms")
    lines.append("")
    lines.append("## Single Replica (`N=1`)")
    lines.append("")
    lines.append("| Service | p95 (ms) | p99 (ms) | Error Rate (%) | Result |")
    lines.append("|---|---:|---:|---:|---|")
    for service in services:
        item = single_by_service[service]
        lines.append(f"| {service} | {item['p95_ms']} | {item['p99_ms']} | {item['error_rate_percent']} | {item['result']} |")

    lines.append("")
    lines.append("## Scaled (`N=3`)")
    lines.append("")
    lines.append("| Service | p95 (ms) | p99 (ms) | Error Rate (%) | Result |")
    lines.append("|---|---:|---:|---:|---|")
    for service in services:
        item = scaled_by_service[service]
        lines.append(f"| {service} | {item['p95_ms']} | {item['p99_ms']} | {item['error_rate_percent']} | {item['result']} |")

    lines.append("")
    lines.append("## Scaling Regression Check")
    lines.append("")
    lines.append("| Service | p95 Regression (%) | p99 Regression (%) | Allowed p95/p99 (%) | Result |")
    lines.append("|---|---:|---:|---:|---|")

    for service in services:
        base = single_by_service[service]
        scale = scaled_by_service[service]

        # Use a floor for very small baselines so sub-millisecond shifts do not
        # create outsized percentage regressions and false gate failures.
        base_p95 = max(float(base["p95_ms"]), baseline_floor_ms)
        base_p99 = max(float(base["p99_ms"]), baseline_floor_ms)

        p95_reg = ((float(scale["p95_ms"]) - base_p95) / base_p95) * 100.0
        p99_reg = ((float(scale["p99_ms"]) - base_p99) / base_p99) * 100.0

        passed = (
            p95_reg <= p95_limit
            and p99_reg <= p99_limit
            and base["result"] == "PASS"
            and scale["result"] == "PASS"
        )
        if not passed:
            all_passed = False

        lines.append(
            f"| {service} | {p95_reg:.2f} | {p99_reg:.2f} | {p95_limit:.0f} / {p99_limit:.0f} | {'PASS' if passed else 'FAIL'} |"
        )

    lines.append("")
    lines.append(f"- Release gate decision: **{'PASS' if all_passed else 'FAIL'}**")

    SUMMARY_PATH.write_text("\n".join(lines) + "\n", encoding="utf-8")

    if not all_passed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
