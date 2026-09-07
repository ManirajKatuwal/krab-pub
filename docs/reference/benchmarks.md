# Benchmarks

This document covers Krab's benchmark and non-functional-test (NFT) methodology,
the thresholds that gate releases, the result snapshots committed to the
repository, and how to reproduce every number locally.

Two things this page is **not**: it is not a marketing comparison, and it does
not contain any number that is not in a committed artifact under
[`benchmarks/`](../../benchmarks/). Committed snapshots are point-in-time
evidence from a specific machine; the thresholds are the contract, the
snapshots are proof the harness runs.

---

## What is measured

Three harnesses live in [`scripts/`](../../scripts/), each with a distinct
purpose and output:

| Harness | Measures | Output | Tracked | Gating |
|---|---|---|---|---|
| [`nft_multi_service_gate.py`](../../scripts/nft_multi_service_gate.py) | p50/p95/p99 latency and error rate for `service_frontend`, `service_auth`, `service_users` under `load` / `spike` / `soak` profiles | `latest_summary*.md`, `*_replica_results.json` | No (CI artifact) | **Release-blocking** against [`thresholds.json`](../../benchmarks/thresholds.json) |
| [`nft_benchmark_runner.py`](../../scripts/nft_benchmark_runner.py) | Per-route latency for Krab and (optionally) external frameworks, per [`benchmark_config.json`](../../benchmarks/benchmark_config.json) | `external_results.json`, `external_summary.md` | **Yes** (committed snapshots of the latest run) | Informational only |
| [`targeted_hardening_checks.py`](../../scripts/targeted_hardening_checks.py) | `service_auth` health-endpoint latency under load, plus a login-endpoint fuzz (asserting no 5xx) | `targeted_hardening_results.json` | **Yes** | Informational only |

Supporting automation:

- [`nft_scaling_compare.py`](../../scripts/nft_scaling_compare.py) — evaluates
  the `N=1` vs `N=3` horizontal-scaling regression gate.
- [`nft_append_trends.py`](../../scripts/nft_append_trends.py) — appends one
  row per service/profile/replica mode to
  [`trend_history.csv`](../../benchmarks/trend_history.csv) (append-only; rows
  are never rewritten).
- [`shared_state_validation.py`](../../scripts/shared_state_validation.py) —
  verifies that rate limiting still blocks (HTTP 429) under the scaled topology
  with Redis-backed shared state. With `KRAB_SHARED_STATE_SCENARIO=auth_failure`
  it drives invalid tokens instead and, when
  `KRAB_SHARED_STATE_MAX_BLOCK_INDEX` is set, requires the first block to land
  at or below that request index — which is what separates a block by the
  per-IP auth-failure limiter from one by the global token bucket.
  `KRAB_SHARED_STATE_CLIENT_IP` sends that address as `X-Forwarded-For` so the
  scenario runs against counters no earlier scenario in the job has spent; both
  limiters key on the client IP, and a token bucket left partly drained by a
  previous step blocks *earlier* than its capacity, which would satisfy the
  bound without the limiter under test doing anything. It is honoured only
  where the target trusts forwarded headers.
- [`release_evidence_bundle.py`](../../scripts/release_evidence_bundle.py) —
  consolidates run artifacts into a release-evidence manifest.

All latency figures are wall-clock milliseconds measured client-side by the
harness. **The release-blocking gate issues requests serially** — one in
flight at a time (`nft_multi_service_gate.py` loops over `urllib` calls) — so
its thresholds bound sequential, unloaded latency, not latency under concurrent
load; gate-level concurrency is open work in the benchmark plan.
**Latency percentiles are computed over successful requests only** —
failed requests are counted in the error rate and excluded from the latency
samples. A run with a high error rate therefore has percentiles that describe
only the requests that got through; always read the error-rate column first.

---

## Thresholds and gating

[`benchmarks/thresholds.json`](../../benchmarks/thresholds.json) is the source
of truth for release-blocking limits. Per service, identical across the `load`
(1,000 samples), `spike` (3,000), and `soak` (10,000) profiles:

| Service | p95 hard limit | p99 hard limit | Max regression vs baseline |
|---|---:|---:|---|
| `service_frontend` | 120 ms | 250 ms | +20% (p95 and p99), 14-day baseline window |
| `service_auth` | 80 ms | 160 ms | +20% (p95 and p99), 14-day baseline window |
| `service_users` | 150 ms | 300 ms | +20% (p95 and p99), 14-day baseline window |

### Horizontal scaling gate (`N=1` vs `N=3`)

The [`nft.yaml`](../../.github/workflows/nft.yaml) workflow runs the full suite
twice — once with a single replica of each service, once with three replicas of
each — inside the internal-network compose topology
(`docker-compose.nft.yaml`), with Redis-backed shared state (`KRAB_REDIS_URL`).
The scaled run must satisfy, relative to the single-replica run:

| Gate | Limit |
|---|---|
| p95 increase from `N=1` | ≤ 15% |
| p99 increase from `N=1` | ≤ 20% |
| Error rate | ≤ 0.5% |
| Shared state | Required (`redis`) — the run fails if scaled validation executes without it |

The suite is expensive, so it runs only on pushes to `main` or on pull requests
carrying the `nft` label. CI uploads run outputs as workflow artifacts rather
than committing them, and appends one row per service/profile/replica mode to
`trend_history.csv`. A run fails if any hard threshold is exceeded, any
regression limit is exceeded, the scaling gate fails, or shared-state
validation runs without shared state enabled.

---

## External comparison methodology

[`benchmark_config.json`](../../benchmarks/benchmark_config.json) defines the
comparison matrix the runner supports:

- **Frameworks (7):** Krab (Rust), Next.js (React), Nuxt (Vue), SvelteKit,
  Remix (React), Leptos (Rust), Dioxus (Rust) — each addressed by a
  `BENCH_*_URL` environment variable.
- **Routes (5):** `home` (`/`), `blog_post` (`/blog/benchmark-test`),
  `api_status` (`/api/status`), `api_mutation` (`POST /api/contact`),
  `health` (`/health`).
- **Profiles (3):**

| Profile | Samples | Concurrency | Warmup |
|---|---:|---:|---:|
| `load` | 1,000 | 10 | 5 s |
| `spike` | 3,000 | 50 | 5 s |
| `soak` | 10,000 | 20 | 10 s |

The runner reports p50, p95, p99, mean, stddev, max, and error rate per route,
and writes both a JSON artifact and a Markdown summary. External-framework
results are **informational**: they never block a release, and when appended to
the trend history they carry `result = info`.

Two methodology caveats are built into the harness itself:

1. **Krab's default rate limiter throttles the harness.** The runner warns at
   startup that `service_frontend`'s default limit (60 rps) will produce
   artificially high error rates unless the service is started with the
   per-profile `KRAB_RATE_LIMIT_REFILL_PER_SEC` / `KRAB_RATE_LIMIT_CAPACITY`
   values listed in `benchmark_config.json`.
2. **Release-profile parity (fairness rule).** Until 2026-08-08 the workspace
   built *all* release artifacts at `opt-level = "z"` (optimise for size),
   including the server binaries under test. Any Krab-vs-others comparison run
   before that date measured a size-optimised build against speed-optimised
   competitors and understated Krab. The profile is now split — servers at
   `opt-level = 3`, the WASM client at `"z"` — and **any pre-2026-08-08
   external comparison is non-comparable and must be discarded** for
   comparison purposes.

---

## Committed results

Everything below is transcribed from artifacts committed under
[`benchmarks/`](../../benchmarks/). No other numbers exist in the repository.

### External benchmark snapshot — 2026-03-08 (Krab only)

From [`external_results.json`](../../benchmarks/external_results.json) /
[`external_summary.md`](../../benchmarks/external_summary.md), run
2026-03-08T11:26:36Z, `load` profile (1,000 samples, concurrency 10), all
values in milliseconds:

| Route | p50 | p95 | p99 | Mean | Stddev | Max | Error % |
|---|---:|---:|---:|---:|---:|---:|---:|
| `home` | 3.90 | 6.16 | 9.85 | 4.13 | 1.69 | 25.20 | 0.0 |
| `blog_post` | 3.70 | 5.78 | 8.53 | 3.91 | 1.40 | 19.45 | 0.0 |
| `api_status` | 306.85 | 372.69 | 439.12 | 311.15 | 34.97 | 516.73 | 0.0 |
| `api_mutation` | 6.53 | 10.82 | 20.08 | 6.98 | 2.57 | 26.30 | 0.0 |
| `health` | 5.46 | 8.62 | 11.52 | 5.68 | 1.79 | 17.50 | 0.0 |

Read this snapshot with all of the following caveats:

- **It contains Krab only.** Although the harness and config support seven
  frameworks, no external-framework numbers have ever been committed. There is
  no committed evidence supporting any Krab-vs-X comparison claim today.
- **It ran on a local development machine** (the trend history records the
  commit for this period as `local-dev`), not a controlled or documented host.
- **It pre-dates the 2026-08-08 release-profile split**, so it measured a
  size-optimised (`opt-level = "z"`) build. It is retained as evidence that
  the harness produces complete artifacts end-to-end — not as a performance
  claim, and not as a baseline for comparison.
- `api_status` is markedly slower than every other route in this run
  (p50 306.85 ms vs single-digit p50 elsewhere). The artifact records no
  explanation; do not extrapolate one.

### Targeted hardening snapshot — 2026-04-05

From [`targeted_hardening_results.json`](../../benchmarks/targeted_hardening_results.json),
run 2026-04-05T10:30:22Z against `service_auth` on `127.0.0.1:3001`:

**Load — `GET /health`** (120 samples, concurrency 8):

| Samples | Success | Errors | p50 | p95 | p99 | Mean |
|---:|---:|---:|---:|---:|---:|---:|
| 120 | 119 | 1 | 6.69 ms | 11.37 ms | 16.19 ms | 7.07 ms |

**Fuzz — `POST /api/v1/auth/login`** (120 random-payload samples):

| Samples | 2xx | 4xx | 5xx | Transport/other |
|---:|---:|---:|---:|---:|
| 120 | 0 | 120 | 0 | 0 |

The fuzz result is the interesting one: 120 malformed login payloads all
produced client errors — no server errors and no transport failures. The load
figures share the local-machine caveat above.

### Trend history

[`trend_history.csv`](../../benchmarks/trend_history.csv) is append-only and
currently holds three groups of rows. Every committed row records its commit as
`local-dev`.

**2026-02-27 — seeded baseline.** Initial `service_frontend` rows for all
three profiles, seeded from a local non-functional test profile (p95 0–1 ms
recorded; these are seed values, not a measured run of significance).

**2026-03-08T10:53:42Z — external-benchmark rows (`result = info`).** A run of
the external harness *earlier the same day* than the committed snapshot above
— these rows and the snapshot are different runs:

| Route | p95 (ms) | p99 (ms) | Error % |
|---|---:|---:|---:|
| `home` | 6.37 | 8.36 | 88.0 |
| `blog_post` | 9.74 | 15.85 | 97.2 |
| `api_status` | 2156.87 | 2348.37 | 0.0 |
| `api_mutation` | 16.19 | 16.19 | 98.4 |
| `health` | 9.27 | 16.75 | 94.3 |

The 88–98% error rates match the failure mode the runner explicitly warns
about — Krab's default rate limiter throttling the harness — though the rows
themselves record no cause. Because percentiles cover successful requests
only, the latency columns of these rows describe a small fraction of the
traffic. These rows are marked `info` and never gated anything.

**2026-03-18 — composite service runs, `N=1` and `N=3`, Redis shared state.**

| Service | Mode | p95 (ms) | p99 (ms) | Threshold p95/p99 | Result |
|---|---|---:|---:|---|---|
| `service_frontend` | single | 7 | 8 | 120 / 250 | pass |
| `service_auth` | single | 8 | 9 | 80 / 160 | pass |
| `service_users` | single | 6 | 9 | 150 / 300 | pass |
| `service_frontend` | scaled | 8 | 10 | 120 / 250 | pass |
| `service_auth` | scaled | 8 | 11 | 80 / 160 | pass |
| `service_users` | scaled | 8 | 10 | 150 / 300 | pass |

All rows pass their thresholds with wide margins, and the scaled runs sit
within the scaling-gate limits relative to the single-replica runs. The rows
are annotated `ci-nft-auto-append` but record commit `local-dev`.

### What these numbers do and do not mean

- They prove the harnesses, gates, and artifact pipeline work end-to-end, and
  they establish that Krab's reference services passed their own SLO
  thresholds on the machine that ran them, on the date recorded.
- They do **not** transfer to other hardware, do not describe current `main`
  (they pre-date later optimisation work, including the release-profile
  split), and do not support any cross-framework claim.
- The thresholds in `thresholds.json` are the durable contract; a fresh run on
  your hardware against those thresholds tells you more than any committed
  snapshot.

---

## Reproducing locally

All harnesses are plain Python 3 (standard library only) run from the
repository root.

### External benchmark runner

Start `service_frontend` with the rate limiter raised to the profile's values
(from `benchmark_config.json`; shown here for `load`), then run the runner:

```sh
# terminal 1
KRAB_RATE_LIMIT_REFILL_PER_SEC=2000 KRAB_RATE_LIMIT_CAPACITY=4000 \
  cargo run --release --bin service_frontend

# terminal 2
python scripts/nft_benchmark_runner.py --frameworks krab --profile load
```

Omitting `--frameworks` targets every framework in the config; point
`BENCH_NEXTJS_URL` etc. at running instances first. **This overwrites the
tracked snapshots** `benchmarks/external_results.json` and
`benchmarks/external_summary.md` — they are committed copies of the latest
run, so either commit the new snapshot deliberately (with environment notes)
or restore the files.

### Multi-service NFT gate

CI runs this inside the compose topology; locally you can run it against
already-running services (`service_frontend` :3000, `service_auth` :3001,
`service_users` :3002, overridable via `KRAB_FRONTEND_BASE_URL`,
`KRAB_AUTH_BASE_URL`, `KRAB_USERS_BASE_URL`):

```sh
python scripts/nft_multi_service_gate.py --mode single \
  --json-out benchmarks/single_replica_results.json \
  --markdown-out benchmarks/latest_summary_single.md
```

Then the scaling comparison and trend append, exactly as
[`nft.yaml`](../../.github/workflows/nft.yaml) invokes them:

```sh
python scripts/nft_scaling_compare.py \
  benchmarks/single_replica_results.json \
  benchmarks/scaled_replica_results.json

KRAB_SHARED_STATE_MODE=redis python scripts/nft_append_trends.py \
  benchmarks/single_replica_results.json \
  benchmarks/scaled_replica_results.json
```

For the full replicated topology, use the compose files CI uses
(`docker-compose.yml` + `docker-compose.nft.yaml`, `--scale service_*=3`); the
workflow file is the authoritative sequence.

### Targeted hardening checks

Self-contained — builds and starts `service_auth` on `:3001` itself, runs the
load and fuzz passes, then shuts it down:

```sh
python scripts/targeted_hardening_checks.py
```

This overwrites the tracked `benchmarks/targeted_hardening_results.json`.

### Which outputs are tracked

[`benchmarks/README.md`](../../benchmarks/README.md) holds the authoritative
table — check it before adding any artifact. Summary:

| Tracked (committed) | Generated (gitignored, CI-artifact only) |
|---|---|
| `thresholds.json`, `benchmark_config.json`, `trend_history.csv` | `latest_summary.md`, `latest_summary_single.md`, `latest_summary_scaled.md` |
| `external_results.json`, `external_summary.md` | `single_replica_results.json`, `scaled_replica_results.json` |
| `targeted_hardening_results.json` | `shared_state_validation.json`, `shared_state_auth_failure_validation.json`, `release_evidence_bundle.json` / `.md` |

Never write benchmark output to the repository root; generated artifacts
belong under the gitignored patterns above or in `internal/`.
