# Deployment Guide

This document covers deployment patterns for Krab services in containerized and self-hosted environments.

---

## Deployment Targets

Krab is designed for:

- **Containerized environments**: Docker, Kubernetes, Docker Swarm
- **Self-hosted**: Bare metal or VPS
- **Small hosts**: the services are single Tokio/Axum binaries. There is no AWS Lambda or Cloudflare Workers adapter; running there is not supported today

---

## Container Build

### Dockerfile (multi-stage)

```dockerfile
# Build stage
FROM rust:1.89-slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin service_auth
RUN cargo build --release --bin service_users
RUN cargo build --release --bin service_frontend

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/service_auth /usr/local/bin/
COPY --from=builder /app/target/release/service_users /usr/local/bin/
COPY --from=builder /app/target/release/service_frontend /usr/local/bin/
COPY --from=builder /app/services/service_frontend/public /app/public

EXPOSE 3000 3001 3002
```

This image carries no browser bundle. `service_frontend` links
`/pkg/service_frontend_islands.js` but does not serve it: build
`services/service_frontend_islands` with `wasm-pack build … --target web --
--features web`, serve the output directory (including `snippets/`) at `/pkg/`,
and point `KRAB_FRONTEND_PKG_DIR` at it so the asset manifest can publish the
bundle's integrity digest. The repository's [`Dockerfile.service`](../../Dockerfile.service)
(one binary per image, `BIN` build argument) is the maintained variant.

### Per-service images (recommended for production)

Build separate images for each service to enable independent scaling:

```dockerfile
# service_auth.Dockerfile
FROM rust:1.89-slim-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin service_auth

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/service_auth /usr/local/bin/
EXPOSE 3001
CMD ["service_auth"]
```

---

## Docker Compose (local development)

```yaml
version: "3.8"

services:
  postgres:
    image: postgres:15
    environment:
      POSTGRES_DB: krab_users
      POSTGRES_HOST_AUTH_METHOD: trust
    ports:
      - "5432:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    ports:
      - "6379:6379"

  service_auth:
    build: { context: ., dockerfile: service_auth.Dockerfile }
    environment:
      KRAB_ENVIRONMENT: dev
      KRAB_AUTH_MODE: jwt
      KRAB_OIDC_ISSUER: krab.auth
      KRAB_OIDC_AUDIENCE: krab.services
      KRAB_HOST: 0.0.0.0
      KRAB_PORT: 3001
      KRAB_JWT_SECRET: ${JWT_SECRET}
      KRAB_REDIS_URL: redis://redis:6379
    ports:
      - "3001:3001"

  service_users:
    build: { context: ., dockerfile: service_users.Dockerfile }
    environment:
      KRAB_ENVIRONMENT: dev
      KRAB_DB_DRIVER: postgres
      DATABASE_URL: postgres://postgres@postgres:5432/krab_users
      KRAB_HOST: 0.0.0.0
      KRAB_PORT: 3002
    ports:
      - "3002:3002"
    depends_on:
      - postgres

  service_frontend:
    build: { context: ., dockerfile: service_frontend.Dockerfile }
    environment:
      KRAB_ENVIRONMENT: dev
      KRAB_HOST: 0.0.0.0
      KRAB_PORT: 3000
    ports:
      - "3000:3000"

volumes:
  pgdata:
```

---

## Kubernetes Deployment

### Secret management

Mount secrets as files using Kubernetes secrets:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: krab-auth-secrets
type: Opaque
data:
  jwt-secret: <base64-encoded-secret>
  bootstrap-password: <base64-encoded-password>
```

Reference in the deployment:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: service-auth
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: service-auth
          image: your-registry/krab-service-auth:latest
          env:
            - name: KRAB_ENVIRONMENT
              value: "prod"
            - name: KRAB_AUTH_MODE
              value: "jwt"
            - name: KRAB_OIDC_ISSUER
              value: "https://auth.example.com"
            - name: KRAB_OIDC_AUDIENCE
              value: "krab-api"
            - name: KRAB_JWT_SECRET_FILE
              value: "/run/secrets/jwt-secret"
            # Must contain an Argon2id PHC hash (`krab auth hash-password`),
            # not a plaintext password: prod rejects anything else.
            - name: KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE
              value: "/run/secrets/bootstrap-password"
            # Required outside dev: startup fails without an explicit list.
            - name: KRAB_CORS_ORIGINS
              value: "https://app.example.com"
            - name: KRAB_HOST
              value: "0.0.0.0"
            - name: KRAB_PORT
              value: "3001"
          volumeMounts:
            - name: secrets
              mountPath: /run/secrets
              readOnly: true
          ports:
            - containerPort: 3001
          livenessProbe:
            httpGet:
              path: /health
              port: 3001
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /ready
              port: 3001
            initialDelaySeconds: 5
            periodSeconds: 10
      volumes:
        - name: secrets
          secret:
            secretName: krab-auth-secrets
```

### Horizontal scaling

For multi-replica deployments, use shared Redis for rate limiting and auth state:

```yaml
- name: KRAB_REDIS_URL_FILE
  value: "/run/secrets/krab/redis_url"   # file contents: redis://redis-service:6379
```

The Redis URL is a policy-checked secret: an inline `KRAB_REDIS_URL` is rejected
at startup in `prod`, so mount it as a file and point `KRAB_REDIS_URL_FILE` at it.

---

## Health Checks

The services expose standardized health and readiness endpoints (`service_frontend` and `service_users_split` answer `/ready` without dependency checks — see [api.md §2](api.md#2-standard-service-endpoints)):

| Endpoint | Purpose | Use |
|---|---|---|
| `GET /health` | Liveness check | Kubernetes `livenessProbe`, load balancer health |
| `GET /ready` | Readiness with dependency status | Kubernetes `readinessProbe`, traffic routing |
| `GET /metrics/prometheus` | Prometheus-compatible metrics — **requires auth** unless `KRAB_METRICS_PUBLIC=true` | Monitoring stack scraping |

### Readiness response example

```json
{
  "status": "ready",
  "uptime_seconds": 3600,
  "dependencies": [
    {
      "name": "postgres",
      "ready": true,
      "critical": true,
      "latency_ms": null,
      "detail": "connection-pool-available"
    }
  ]
}
```

---

## Environment Promotion

Krab enforces a strict promotion order for database migrations:

```
local → dev → staging → prod
```

- Backward migrations (e.g., `prod` → `dev`) are rejected.
- Skipping stages triggers warnings.
- Release environments (`staging`, `prod`) require rollback rehearsal evidence before migration application.

---

## Monitoring Integration

### Prometheus

Scrape configuration:

```yaml
scrape_configs:
  - job_name: 'krab-services'
    static_configs:
      - targets:
          - 'service-auth:3001'
          - 'service-users:3002'
          - 'service-frontend:3000'
    metrics_path: '/metrics/prometheus'
    scrape_interval: 15s
    # The metrics endpoints require a bearer token unless the service runs
    # with KRAB_METRICS_PUBLIC=true.
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/krab-scrape-token
```

`service_frontend` serves the same two metrics routes as the other services
from 0.6.0 (before that it had none, and a scrape of it collected nothing).

### Key metrics

| Metric | Type | Description |
|---|---|---|
| `krab_requests_total` | Counter | Total HTTP requests (unlabelled; per-protocol and per-class breakdowns below) |
| `krab_http_requests_by_protocol_total` | Counter | Requests by resolved `protocol` |
| `krab_http_responses_total` | Counter | Responses by status `class` (`2xx`, `4xx`, `5xx`) |
| `krab_response_2xx_total`, `krab_response_4xx_total`, `krab_response_5xx_total` | Counter | The same counts as separate unlabelled series |
| `krab_http_responses_by_protocol_total`, `krab_http_responses_by_protocol_and_class_total` | Counter | Responses by `protocol` and `class` (identical series under two names) |
| `krab_http_request_duration_seconds` | Histogram | Request latency — `_bucket`, `_sum`, `_count`. Named `krab_request_duration_seconds` (buckets only) before 0.6.0; that alias is still emitted and is removed in 0.7.0 |
| `krab_inflight_requests` | Gauge | Requests currently being handled |
| `krab_auth_failures_total` | Counter | Authentication failures |
| `krab_auth_failures_by_reason_total` | Counter | Authentication failures by `reason` (see [security.md](security.md)) |
| `krab_readiness_status` | Gauge | The verdict of the most recent `/ready` call (`1`/`0`). Starts at `1` and only changes when `/ready` is called. Only `service_auth` and `service_users` compute it (their `/ready` is `readiness_with_dependencies`). `service_frontend` and `service_users_split` export it too, but their `/ready` checks no dependency and always answers `ready`, so for them the gauge is a constant `1` — it agrees with `/ready`, and `ServiceNotReady` cannot fire for those two jobs. Giving them a real verdict means choosing which of their upstreams are critical, which 0.6.0 does not do |
| `krab_uptime_seconds` | Gauge | Seconds since the process started |

For SLO targets and alert configuration, see [`docs/operations/slo_alerts.md`](../operations/slo_alerts.md).

---

## Protocol Flexibility Deployment Configuration

Use protocol controls to tune service exposure and deployment topology.

### Single-service topology (default)

```env
KRAB_PROTOCOL_TOPOLOGY=single_service
KRAB_PROTOCOL_EXPOSURE_MODE=single
KRAB_PROTOCOL_ENABLED=rest
KRAB_PROTOCOL_DEFAULT=rest
```

### Split-services topology

```env
KRAB_PROTOCOL_TOPOLOGY=split_services
KRAB_PROTOCOL_EXPOSURE_MODE=multi
KRAB_PROTOCOL_ENABLED=rest,graphql,rpc
KRAB_PROTOCOL_DEFAULT=rest
KRAB_PROTOCOL_SPLIT_TARGETS_JSON={"users":{"rest":"http://users-rest:3002","graphql":"http://users-graphql:3002","rpc":"http://users-rpc:3002"}}
```

### Per-service protocol controls

- `KRAB_PROTOCOL_ENABLED=rest|graphql|rpc` (CSV)
- `KRAB_PROTOCOL_EXPOSURE_MODE=single|multi`
- `KRAB_PROTOCOL_DEFAULT=rest|graphql|rpc` (must be in enabled set)

These are evaluated per process, so different services can run different policy envelopes.

---

## Pre-deployment Checklist

Before deploying to production:

- [ ] `cargo deny --all-features check advisories bans licenses sources` passes
- [ ] `KRAB_ENVIRONMENT=prod` is set
- [ ] All secrets use `*_FILE` sourcing (no inline secrets; `*_VAULT_REF` has no runtime resolver and fails startup)
- [ ] `KRAB_AUTH_MODE=jwt` or `oidc` (not `static`)
- [ ] Database credentials are rotated from defaults
- [ ] Health and readiness probes are configured
- [ ] Prometheus scraping is configured
- [ ] Rollback rehearsal evidence exists for the current migration version
- [ ] CORS origins are explicitly configured (`KRAB_CORS_ORIGINS`)
