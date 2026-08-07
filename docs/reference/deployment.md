# Deployment Guide

This document covers deployment patterns for Krab services in containerized and self-hosted environments.

---

## Deployment Targets

Krab is designed for:

- **Containerized environments**: Docker, Kubernetes, Docker Swarm
- **Self-hosted**: Bare metal or VPS
- **Edge-ready**: Can run on limited-resource environments (AWS Lambda, Cloudflare Workers)

---

## Container Build

### Dockerfile (multi-stage)

```dockerfile
# Build stage
FROM rust:1.75-slim AS builder
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
COPY --from=builder /app/service_frontend/public /app/public

EXPOSE 3000 3001 3002
```

### Per-service images (recommended for production)

Build separate images for each service to enable independent scaling:

```dockerfile
# service_auth.Dockerfile
FROM rust:1.75-slim AS builder
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
            - name: KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE
              value: "/run/secrets/bootstrap-password"
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
- name: KRAB_REDIS_URL
  value: "redis://redis-service:6379"
```

---

## Health Checks

All services expose standardized health and readiness endpoints:

| Endpoint | Purpose | Use |
|---|---|---|
| `GET /health` | Liveness check | Kubernetes `livenessProbe`, load balancer health |
| `GET /ready` | Readiness with dependency status | Kubernetes `readinessProbe`, traffic routing |
| `GET /metrics/prometheus` | Prometheus-compatible metrics | Monitoring stack scraping |

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
```

### Key metrics

| Metric | Type | Description |
|---|---|---|
| `krab_requests_total` | Counter | Total HTTP requests by service, method, path, status |
| `krab_request_duration_seconds` | Histogram | Request latency distribution |
| `krab_active_connections` | Gauge | Current active connections |

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

- [ ] `cargo deny --all-features check advisories licenses bans` passes
- [ ] `KRAB_ENVIRONMENT=prod` is set
- [ ] All secrets use `*_FILE` or `*_VAULT_REF` sourcing (no inline secrets)
- [ ] `KRAB_AUTH_MODE=jwt` or `oidc` (not `static`)
- [ ] Database credentials are rotated from defaults
- [ ] Health and readiness probes are configured
- [ ] Prometheus scraping is configured
- [ ] Rollback rehearsal evidence exists for the current migration version
- [ ] CORS origins are explicitly configured (`KRAB_CORS_ORIGINS`)
