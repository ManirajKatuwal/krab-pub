# Benchmark Results (load)

- **Timestamp:** 2026-03-08T11:26:36Z
- **Profile:** load (Samples: 1000, Concurrency: 10)

## Latency Comparison (p95 ms)

| Route | Krab (Rust) |
|---|---:|---:|---:|---:|---:|---:|---:|
| `home` | 6.16 |
| `blog_post` | 5.78 |
| `api_status` | 372.69 |
| `api_mutation` | 10.82 |
| `health` | 8.62 |

## Detailed Metrics
### Krab (Rust)
| Route | p50 | p95 | p99 | Mean | Max | Error % |
|---|---:|---:|---:|---:|---:|---:|
| home | 3.9 | 6.16 | 9.85 | 4.13 | 25.2 | 0.0 |
| blog_post | 3.7 | 5.78 | 8.53 | 3.91 | 19.45 | 0.0 |
| api_status | 306.85 | 372.69 | 439.12 | 311.15 | 516.73 | 0.0 |
| api_mutation | 6.53 | 10.82 | 20.08 | 6.98 | 26.3 | 0.0 |
| health | 5.46 | 8.62 | 11.52 | 5.68 | 17.5 | 0.0 |
