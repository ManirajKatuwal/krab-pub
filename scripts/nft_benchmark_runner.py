import json
import math
import os
import pathlib
import sys
import time
import urllib.error
import urllib.request
import statistics
import concurrent.futures
from typing import List, Dict, Any

# Paths
ROOT = pathlib.Path(__file__).resolve().parents[1]
CONFIG_PATH = ROOT / "benchmarks" / "benchmark_config.json"
RESULTS_DIR = ROOT / "benchmarks"
RESULTS_JSON = RESULTS_DIR / "external_results.json"
RESULTS_MD = RESULTS_DIR / "external_summary.md"

def load_config() -> Dict[str, Any]:
    return json.loads(CONFIG_PATH.read_text(encoding="utf-8"))

def env(name: str, default: str) -> str:
    val = os.getenv(name)
    return val.strip() if val and val.strip() else default

def percentile(values: List[float], pct: float) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    index = int(math.ceil((pct / 100.0) * len(sorted_values))) - 1
    index = max(0, min(index, len(sorted_values) - 1))
    return sorted_values[index]

def run_request(url: str, method: str, body: Dict = None) -> float:
    start = time.perf_counter()
    try:
        req = urllib.request.Request(url, method=method)
        if body:
            req.add_header("Content-Type", "application/json")
            req.data = json.dumps(body).encode("utf-8")
        
        with urllib.request.urlopen(req, timeout=5) as resp:
            resp.read() # Consume body
            status = resp.status
            if not (200 <= status < 300):
                raise Exception(f"HTTP {status}")
    except Exception as e:
        # Penalize errors with a high latency or re-raise based on policy
        # For this benchmark, we'll return None to signal error
        return None
    
    return (time.perf_counter() - start) * 1000.0

def benchmark_route(base_url: str, route_path: str, method: str, profile_config: Dict) -> Dict:
    url = f"{base_url.rstrip('/')}{route_path}"
    samples = profile_config.get("samples", 100)
    concurrency = profile_config.get("concurrency", 1)
    
    latencies = []
    errors = 0
    
    # Simple thread pool for concurrency
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = []
        # Submit all tasks
        for _ in range(samples):
            # For mutation route, send a dummy payload
            body = {"name": "Bench", "email": "bench@test.com", "message": "hello"} if method == "POST" else None
            futures.append(executor.submit(run_request, url, method, body))
            
        for future in concurrent.futures.as_completed(futures):
            try:
                latency = future.result()
                if latency is None:
                    errors += 1
                else:
                    latencies.append(latency)
            except Exception:
                errors += 1

    if not latencies:
        return {
            "samples": samples,
            "errors": errors,
            "error_rate": 100.0,
            "p50": 0, "p95": 0, "p99": 0, "mean": 0, "stddev": 0, "max": 0
        }

    return {
        "samples": samples,
        "errors": errors,
        "error_rate": round((errors / samples) * 100.0, 2),
        "p50": round(percentile(latencies, 50), 2),
        "p95": round(percentile(latencies, 95), 2),
        "p99": round(percentile(latencies, 99), 2),
        "mean": round(statistics.mean(latencies), 2),
        "stddev": round(statistics.stdev(latencies), 2) if len(latencies) > 1 else 0,
        "max": round(max(latencies), 2)
    }

def main():
    config = load_config()
    
    # Parse args (simple manual parsing)
    target_frameworks = []
    target_profile = "load"
    
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--frameworks":
            target_frameworks = args[i+1].split(",")
            i += 2
        elif args[i] == "--profile":
            target_profile = args[i+1]
            i += 2
        else:
            i += 1
            
    if not target_frameworks:
        target_frameworks = list(config["frameworks"].keys())

    profile_config = config["profiles"].get(target_profile, config["profiles"]["load"])

    print(f"Running benchmark profile '{target_profile}' against: {', '.join(target_frameworks)}")
    print(f"Config: {json.dumps(profile_config)}")

    # Pre-flight: warn if Krab is a target and its rate limiter would throttle the load
    if "krab" in target_frameworks:
        rps = profile_config.get("krab_rate_limit_rps")
        cap = profile_config.get("krab_rate_limit_capacity")
        if rps and cap:
            actual_rps = os.getenv("KRAB_RATE_LIMIT_REFILL_PER_SEC", "")
            actual_cap = os.getenv("KRAB_RATE_LIMIT_CAPACITY", "")
            if not actual_rps or not actual_cap:
                print()
                print("=" * 70)
                print("WARNING: Krab rate limiter defaults (60 rps) will throttle this")
                print(f"         profile and produce artificially high error rates.")
                print(f"         Start service_frontend with:")
                print(f"           KRAB_RATE_LIMIT_REFILL_PER_SEC={rps} \\")
                print(f"           KRAB_RATE_LIMIT_CAPACITY={cap}")
                print("=" * 70)
                print()

    all_results = {}
    timestamp = time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())

    for fw_key in target_frameworks:
        if fw_key not in config["frameworks"]:
            print(f"Skipping unknown framework: {fw_key}")
            continue
            
        fw_conf = config["frameworks"][fw_key]
        base_url = env(fw_conf["base_url_env"], fw_conf["default_base_url"])
        print(f"\nTesting {fw_conf['display_name']} at {base_url}...")
        
        fw_results = {}
        for route_key, route_path in fw_conf["routes"].items():
            method = "POST" if route_key == "api_mutation" else "GET"
            print(f"  - {route_key} ({method} {route_path})", end="...", flush=True)
            
            # Warmup (simple implementation: run a few requests)
            warmup_sec = profile_config.get("warmup_seconds", 0)
            if warmup_sec > 0:
                end_warmup = time.time() + warmup_sec
                while time.time() < end_warmup:
                    run_request(f"{base_url.rstrip('/')}{route_path}", method, 
                              {"name":"w","email":"w@t.c","message":"w"} if method=="POST" else None)
            
            # Benchmark
            metrics = benchmark_route(base_url, route_path, method, profile_config)
            fw_results[route_key] = metrics
            print(f" Done. p95={metrics['p95']}ms, err={metrics['error_rate']}%")
            
        all_results[fw_key] = fw_results

    # Save JSON results
    output = {
        "timestamp_utc": timestamp,
        "profile": target_profile,
        "results": all_results
    }
    RESULTS_JSON.write_text(json.dumps(output, indent=2))
    print(f"\nResults saved to {RESULTS_JSON}")

    # Generate Markdown Summary
    md_lines = [
        f"# Benchmark Results ({target_profile})",
        f"",
        f"- **Timestamp:** {timestamp}",
        f"- **Profile:** {target_profile} (Samples: {profile_config['samples']}, Concurrency: {profile_config['concurrency']})",
        f"",
        "## Latency Comparison (p95 ms)",
        "",
        "| Route | " + " | ".join([config["frameworks"][fw]["display_name"] for fw in target_frameworks]) + " |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]

    routes = ["home", "blog_post", "api_status", "api_mutation", "health"]
    for route in routes:
        row = f"| `{route}` |"
        for fw in target_frameworks:
            metrics = all_results.get(fw, {}).get(route, {})
            val = metrics.get("p95", "-")
            row += f" {val} |"
        md_lines.append(row)

    md_lines.append("")
    md_lines.append("## Detailed Metrics")
    
    for fw in target_frameworks:
        name = config["frameworks"][fw]["display_name"]
        md_lines.append(f"### {name}")
        md_lines.append("| Route | p50 | p95 | p99 | Mean | Max | Error % |")
        md_lines.append("|---|---:|---:|---:|---:|---:|---:|")
        for route in routes:
            m = all_results.get(fw, {}).get(route, {})
            if not m: continue
            md_lines.append(f"| {route} | {m['p50']} | {m['p95']} | {m['p99']} | {m['mean']} | {m['max']} | {m['error_rate']} |")
        md_lines.append("")

    RESULTS_MD.write_text("\n".join(md_lines))
    print(f"Summary saved to {RESULTS_MD}")

if __name__ == "__main__":
    main()
