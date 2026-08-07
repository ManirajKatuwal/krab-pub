import csv
import json
import pathlib
import sys
from typing import Dict, List


ROOT = pathlib.Path(__file__).resolve().parents[1]
TREND_PATH = ROOT / "benchmarks" / "trend_history.csv"


FIELDNAMES = [
    "timestamp_utc",
    "service",
    "framework",  # Added
    "route",      # Added
    "profile",
    "replica_mode",
    "shared_state_mode",
    "samples",
    "p95_ms",
    "p99_ms",
    "threshold_p95_ms",
    "threshold_p99_ms",
    "regression_p95_percent",
    "regression_p99_percent",
    "error_rate_percent",
    "result",
    "commit",
    "notes",
]


def load_json(path: pathlib.Path) -> Dict:
    return json.loads(path.read_text(encoding="utf-8"))


def load_existing_rows(path: pathlib.Path) -> List[Dict[str, str]]:
    if not path.exists():
        return []

    with path.open("r", encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle)
        rows = []
        for row in reader:
            migrated = {
                "timestamp_utc": row.get("timestamp_utc", ""),
                "service": row.get("service", ""),
                "framework": row.get("framework", "krab" if row.get("service") else ""),
                "route": row.get("route", "composite"),
                "profile": row.get("profile", "load"),
                "replica_mode": row.get("replica_mode", "single"),
                "shared_state_mode": row.get("shared_state_mode", "memory"),
                "samples": row.get("samples", "0"),
                "p95_ms": row.get("p95_ms", "0"),
                "p99_ms": row.get("p99_ms", "0"),
                "threshold_p95_ms": row.get("threshold_p95_ms", "0"),
                "threshold_p99_ms": row.get("threshold_p99_ms", "0"),
                "regression_p95_percent": row.get("regression_p95_percent", "0"),
                "regression_p99_percent": row.get("regression_p99_percent", "0"),
                "error_rate_percent": row.get("error_rate_percent", "0"),
                "result": row.get("result", "pass"),
                "commit": row.get("commit", "unknown"),
                "notes": row.get("notes", ""),
            }
            rows.append(migrated)
        return rows


def append_rows(single_path: pathlib.Path, scaled_path: pathlib.Path, external_path: pathlib.Path, commit_sha: str, shared_state_mode: str) -> None:
    rows = load_existing_rows(TREND_PATH)

    # 1. Internal Load Tests (Single & Scaled)
    if single_path and single_path.exists() and scaled_path and scaled_path.exists():
        single = load_json(single_path)
        scaled = load_json(scaled_path)
        
        for payload in (single, scaled):
            mode = payload.get("mode", "single")
            timestamp = payload.get("timestamp_utc", "")
            for item in payload.get("results", []):
                rows.append(
                    {
                        "timestamp_utc": timestamp,
                        "service": item.get("service", ""),
                        "framework": "krab",
                        "route": "composite",
                        "profile": "load",
                        "replica_mode": mode,
                        "shared_state_mode": shared_state_mode,
                        "samples": str(item.get("samples", 0)),
                        "p95_ms": str(item.get("p95_ms", 0)),
                        "p99_ms": str(item.get("p99_ms", 0)),
                        "threshold_p95_ms": str(item.get("threshold_p95_ms", 0)),
                        "threshold_p99_ms": str(item.get("threshold_p99_ms", 0)),
                        "regression_p95_percent": "0",
                        "regression_p99_percent": "0",
                        "error_rate_percent": str(item.get("error_rate_percent", 0)),
                        "result": str(item.get("result", "FAIL")).lower(),
                        "commit": commit_sha,
                        "notes": "ci-nft-auto-append",
                    }
                )

    # 2. External Benchmark Results
    if external_path and external_path.exists():
        external = load_json(external_path)
        timestamp = external.get("timestamp_utc", "")
        profile = external.get("profile", "load")
        
        results_map = external.get("results", {})
        for fw_name, routes in results_map.items():
            for route_name, metrics in routes.items():
                rows.append({
                    "timestamp_utc": timestamp,
                    "service": "benchmark",
                    "framework": fw_name,
                    "route": route_name,
                    "profile": profile,
                    "replica_mode": "single",
                    "shared_state_mode": "none",
                    "samples": str(metrics.get("samples", 0)),
                    "p95_ms": str(metrics.get("p95", 0)),
                    "p99_ms": str(metrics.get("p99", 0)),
                    "threshold_p95_ms": "0",
                    "threshold_p99_ms": "0",
                    "regression_p95_percent": "0",
                    "regression_p99_percent": "0",
                    "error_rate_percent": str(metrics.get("error_rate", 0)),
                    "result": "info",
                    "commit": commit_sha,
                    "notes": "external-benchmark",
                })

    with TREND_PATH.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDNAMES)
        writer.writeheader()
        writer.writerows(rows)


def main() -> None:
    # Flexible args: python nft_append_trends.py [single.json scaled.json] [external.json]
    
    single_path = None
    scaled_path = None
    external_path = None
    
    args = sys.argv[1:]
    
    # Heuristic: if 2 args, assume internal. If 1 arg, assume external. If 3 args, assume all.
    if len(args) == 2:
        single_path = pathlib.Path(args[0])
        scaled_path = pathlib.Path(args[1])
    elif len(args) == 1:
        external_path = pathlib.Path(args[0])
    elif len(args) == 3:
        single_path = pathlib.Path(args[0])
        scaled_path = pathlib.Path(args[1])
        external_path = pathlib.Path(args[2])
    else:
         # Fallback to empty run or help? We'll just exit if no recognized pattern
         if len(args) > 0:
             print("usage: nft_append_trends.py <single.json> <scaled.json> [external.json]")
             print("       nft_append_trends.py <external.json>")
             sys.exit(1)

    commit_sha = pathlib.os.environ.get("GITHUB_SHA", "local-dev")[:12]
    shared_state_mode = pathlib.os.environ.get("KRAB_SHARED_STATE_MODE", "redis")

    append_rows(single_path, scaled_path, external_path, commit_sha, shared_state_mode)



if __name__ == "__main__":
    main()
