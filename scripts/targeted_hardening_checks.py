import concurrent.futures
import json
import os
import pathlib
import random
import statistics
import string
import subprocess
import time
import urllib.error
import urllib.request


ROOT = pathlib.Path(__file__).resolve().parents[1]
OUT = ROOT / "benchmarks" / "targeted_hardening_results.json"


def wait_until_ready(url: str, timeout_s: int = 120) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as resp:
                resp.read()
                if 200 <= resp.status < 300:
                    return True
        except Exception:
            time.sleep(0.5)
    return False


def run_load(url: str, samples: int = 120, concurrency: int = 8) -> dict:
    latencies = []
    errors = 0

    def one_request():
        start = time.perf_counter()
        try:
            with urllib.request.urlopen(url, timeout=5) as resp:
                resp.read()
                ok = 200 <= resp.status < 300
        except Exception:
            ok = False
        return ok, (time.perf_counter() - start) * 1000.0

    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as ex:
        futs = [ex.submit(one_request) for _ in range(samples)]
        for fut in concurrent.futures.as_completed(futs):
            ok, latency = fut.result()
            if ok:
                latencies.append(latency)
            else:
                errors += 1

    latencies.sort()

    def pct(p: float) -> float:
        if not latencies:
            return 0.0
        idx = int((p / 100.0) * len(latencies)) - 1
        idx = max(0, min(idx, len(latencies) - 1))
        return latencies[idx]

    return {
        "samples": samples,
        "success": len(latencies),
        "errors": errors,
        "p50_ms": round(pct(50), 2),
        "p95_ms": round(pct(95), 2),
        "p99_ms": round(pct(99), 2),
        "mean_ms": round(statistics.mean(latencies), 2) if latencies else 0.0,
    }


def run_fuzz_login(url: str, samples: int = 120) -> dict:
    ok = 0
    client4xx = 0
    server5xx = 0
    transport_other = 0

    for _ in range(samples):
        body = {
            "username": "".join(random.choice(string.printable) for _ in range(random.randint(0, 32))),
            "password": "".join(random.choice(string.printable) for _ in range(random.randint(0, 64))),
        }
        req = urllib.request.Request(
            url,
            data=json.dumps(body).encode("utf-8"),
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        try:
            with urllib.request.urlopen(req, timeout=3) as resp:
                status = resp.status
                resp.read()
        except urllib.error.HTTPError as exc:
            status = exc.code
        except Exception:
            status = 0

        if 200 <= status < 300:
            ok += 1
        elif 400 <= status < 500:
            client4xx += 1
        elif 500 <= status < 600:
            server5xx += 1
        else:
            transport_other += 1

    return {
        "samples": samples,
        "ok": ok,
        "client4xx": client4xx,
        "server5xx": server5xx,
        "transport_other": transport_other,
    }


def main() -> int:
    env = os.environ.copy()
    env.setdefault("KRAB_ENVIRONMENT", "dev")
    env.setdefault("KRAB_AUTH_MODE", "static")
    env.setdefault("KRAB_BEARER_TOKEN", "benchmark-token")

    subprocess.run(["cargo", "build", "--bin", "service_auth"], cwd=ROOT, env=env, check=False)

    service_bin = ROOT / "target" / "debug" / ("service_auth.exe")
    service_cmd = [str(service_bin)] if service_bin.exists() else ["cargo", "run", "--bin", "service_auth"]

    service = subprocess.Popen(
        service_cmd,
        cwd=ROOT,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        health = "http://127.0.0.1:3001/health"
        login = "http://127.0.0.1:3001/api/v1/auth/login"

        if not wait_until_ready(health, timeout_s=240):
            result = {
                "status": "startup_failed",
                "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            OUT.write_text(json.dumps(result, indent=2), encoding="utf-8")
            print(f"wrote {OUT}")
            return 2

        load_result = run_load(health, samples=120, concurrency=8)
        fuzz_result = run_fuzz_login(login, samples=120)
        result = {
            "status": "ok",
            "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "load": {"target": health, **load_result},
            "fuzz": {"target": login, **fuzz_result},
        }
        OUT.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(f"wrote {OUT}")
        return 0
    finally:
        service.terminate()
        try:
            service.wait(timeout=8)
        except Exception:
            service.kill()
            service.wait(timeout=8)


if __name__ == "__main__":
    raise SystemExit(main())

