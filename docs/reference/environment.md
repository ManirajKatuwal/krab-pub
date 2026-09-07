# Environment Reference

Every environment variable Krab reads, what it accepts, and what happens when it
is unset.

Use [`.env.example`](../../.env.example) as the baseline for local and dev
setup — it is the copy-paste starting point; this page is the authority on
behaviour.

Validate a configuration with:

```bash
cargo run -p krab_cli -- env-check --strict
```

Strict mode fails on warnings as well as errors.

---

## Reading conventions

- **Required** means startup fails without it.
- **Default** is what the runtime uses when the variable is absent or unparsable.
  Where a value is clamped, the bound is given.
- Boolean variables are true only for `1` or `true` (case-insensitive). **Any
  other value — including `yes` and `on` — reads as false.**
- List variables are comma-separated unless noted.

### Secret sourcing

Secrets are read through `krab_core::config::read_env_or_file()`, which resolves
three forms in order:

| Form | Example | Notes |
|---|---|---|
| Inline | `KRAB_JWT_SECRET=…` | **Rejected in `staging` and `prod`** |
| File | `KRAB_JWT_SECRET_FILE=/run/secrets/jwt` | Contents must be non-empty |
| Vault reference | `KRAB_JWT_SECRET_VAULT_REF=kv/data/krab/auth#jwt_secret` | Must resolve at startup |

Any variable marked **secret** below supports the `_FILE` and `_VAULT_REF`
suffixes. In `staging` and `prod`, one of those two suffixed forms is mandatory
and startup rejects an inline value. See
[security reference](security.md).

The suffixed forms are not listed individually in the tables below — every
variable marked **secret** has them.

---

## Core service

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_ENVIRONMENT` | **Yes** | — | `local` \| `dev` \| `staging` \| `prod`. Gates secret-sourcing enforcement and migration promotion policy |
| `KRAB_SERVICE_NAME` | No | per service | **Per-service.** Service identity used in telemetry (the `service` field on every log line and metric), migration records, and the `KRAB_PROTOCOL_ENABLED_<NAME>` lookup. The default is whatever the binary passes to `KrabConfig::from_env_checked` — `frontend`, `auth`, `users`, `users-split` for the reference services, not `krab`. Setting it in a shared environment renames **every** service that inherits it, and it degrades silently: nothing fails, they simply all report one name. Under `krab bootstrap` the orchestrator injects each service's own value from `[services.X].service_name` in `krab.toml` |
| `KRAB_SERVICE` | No | `service` | Fallback service identity for protocol selection when `KRAB_SERVICE_NAME` is unset |
| `KRAB_HOST` | No | `127.0.0.1` | Bind address |
| `KRAB_PORT` | No | per service | **Per-service.** Bind port. The per-service default (`3000` frontend, `3001` auth, `3002` users, `3207` users-split) is a *fallback*, not a floor: when `KRAB_PORT` is set it overrides all of them simultaneously, so every service that inherits it tries to bind the same port. Under `krab bootstrap` the orchestrator injects each service's own value from `[services.X].port` in `krab.toml`, which is what keeps a service on the port its health probe addresses |
| `KRAB_PUBLIC_BASE_URL` | No | `http://localhost:3000` | Public origin for SEO metadata (`canonical`, `og:url`) and `/robots.txt`, `/sitemap.xml`. **Set explicitly to your external HTTPS URL in staging and prod** |
| `RUST_LOG` | No | `info` | `tracing-subscriber` env filter, e.g. `info,krab_core=debug` |

## Authentication

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_AUTH_MODE` | **Yes** | — | `static` \| `jwt`. `static` is for local and dev only |
| `KRAB_BEARER_TOKEN` | Conditional | — | **Secret.** Shared token when `KRAB_AUTH_MODE=static` |
| `KRAB_AUTH_PUBLIC_PATHS` | No | — | Comma-separated paths exempt from auth, **in addition to** the open-path list. Trailing `*` is a prefix match. Everything else is deny-by-default on protected routers |
| `KRAB_AUTH_OPEN_PATHS` | No | built-in list | Comma-separated patterns **replacing** the built-in open (no-auth) path list (`/`, `/health`, `/ready`, auth endpoints, `/blog/*`, `/pkg/*`, …). Trailing `*` is a prefix match. Set it to close default-open paths; an explicitly empty value closes them all. Unset keeps the built-in defaults. Does **not** govern the metrics endpoints — see `KRAB_METRICS_PUBLIC` |
| `KRAB_METRICS_PUBLIC` | No | `false` | Allow anonymous `GET /metrics` and `/metrics/prometheus`. **Off by default:** metrics publish route names, traffic shape, error rates, and latency distributions. Prefer authenticating your scraper or binding metrics to a network only it can reach; set `true` only when that surface is deliberately public. Applies on top of `KRAB_AUTH_OPEN_PATHS`, so it reopens metrics whether or not that list is set |
| `KRAB_AUTH_ADMIN_ROLE` | No | `admin` | Role name granting admin routes |
| `KRAB_AUTH_ADMIN_SCOPE` | No | `admin` | Scope name granting admin routes |
| `KRAB_AUTH_REQUIRED_ROLES` | No | — | Comma-separated roles required on protected routes |
| `KRAB_AUTH_REQUIRED_SCOPES` | No | — | Comma-separated scopes required on protected routes |
| `KRAB_AUTH_REQUIRED_CLAIMS_JSON` | No | — | JSON object of claim/value pairs every token must carry |
| `KRAB_AUTH_ROUTE_POLICIES_JSON` | No | — | JSON map of route pattern → policy, overriding the baseline policy per route |
| `KRAB_AUTH_REQUIRE_TENANT_CLAIM` | No | `false` | Reject any token that carries no tenant claim with `401`. Independent of `KRAB_AUTH_REQUIRE_TENANT_MATCH` |
| `KRAB_AUTH_REQUIRE_TENANT_MATCH` | No | `false` | Require the token tenant claim to match the request tenant |
| `KRAB_AUTH_COOKIE_SESSION_ENABLED` | No | `false` | Enable cookie-backed sessions. **Implies CSRF protection** — enabling this turns on the same protection as `KRAB_CSRF_ENABLED` |
| `KRAB_CSRF_ENABLED` | No | `false` | Enforce CSRF protection on unsafe methods (`POST`, `PUT`, `PATCH`, `DELETE`). Enabled automatically when `KRAB_AUTH_COOKIE_SESSION_ENABLED` is on |
| `KRAB_AUTH_ACCESS_TTL_SECS` | No | service default | Access-token lifetime issued by `service_auth` |
| `KRAB_AUTH_REFRESH_TTL_SECS` | No | service default | Refresh-token lifetime issued by `service_auth` |
| `KRAB_AUTH_BOOTSTRAP_USER` | No | — | Bootstrap account username for `service_auth` |
| `KRAB_AUTH_BOOTSTRAP_PASSWORD` | No | — | **Secret.** Bootstrap account password, as an Argon2id PHC hash. Plaintext is accepted in `dev`/`local` only and hashed at startup |
| `KRAB_AUTH_LOGIN_USERS_JSON` | No | — | **Secret.** JSON object of `username` → Argon2id PHC hash. Generate entries with `krab auth hash-password --username <name>` |
| `KRAB_SERVICE_AUTH_SCOPE` | No | `service:internal` | Scope required for service-to-service calls |
| `KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN` | No | — | **Secret.** Downstream bearer token for frontend to authenticate calls to backend services |
| `KRAB_AUTH_BASE_URL` | No | `http://127.0.0.1:3001` | Auth service base URL for inter-service calls. Overridden by runtime topology when set |
| `KRAB_USERS_BASE_URL` | No | `http://127.0.0.1:3002` | Users service base URL for inter-service calls. Overridden by runtime topology when set |

## JWT and OIDC

Required when `KRAB_AUTH_MODE=jwt`.

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_OIDC_ISSUER` | **Conditional** | — | Expected `iss`. Checked strictly. **Required in staging/prod** when `KRAB_AUTH_MODE=jwt\|oidc` unless every provider in `KRAB_JWT_PROVIDERS_JSON` declares its own `issuer`; startup fails otherwise |
| `KRAB_OIDC_AUDIENCE` | **Conditional** | — | Expected `aud`. Checked strictly. **Required in staging/prod** when `KRAB_AUTH_MODE=jwt\|oidc` unless every provider in `KRAB_JWT_PROVIDERS_JSON` declares its own `audience`; startup fails otherwise |
| `KRAB_JWT_SECRET` | Conditional | — | **Secret.** HMAC signing secret for symmetric algorithms |
| `KRAB_JWT_KEYS_JSON` | Conditional | — | **Secret.** JSON key set for asymmetric verification and KID rotation |
| `KRAB_JWT_PROVIDERS_JSON` | No | — | JSON array of providers, each with `name`, `issuer`, `audience`. Supports `_VAULT_REF` |
| `KRAB_JWT_ACTIVE_KID` | No | — | KID used for newly issued tokens. Older KIDs stay valid for the rotation grace window |
| `KRAB_JWT_REQUIRE_KID` | No | `false` | Reject tokens with no `kid` header |
| `KRAB_JWT_ALLOWED_ALGS` | No | — | Comma-separated allow-list, e.g. `RS256,ES256`. Unset means the built-in allow-list. Mixing HMAC (`HS*`) with asymmetric (`RS*`/`PS*`/`ES*`/`EdDSA`) families is rejected — startup fails outside dev, and the request path refuses to verify (503) in every environment |
| `KRAB_JWT_LEEWAY_SECS` | No | — | Clock-skew tolerance on `exp` and `nbf`. Unset means no leeway |

## Rate limiting, CORS, and proxy trust

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_RATE_LIMIT_CAPACITY` | No | `120` | Token-bucket burst capacity |
| `KRAB_RATE_LIMIT_REFILL_PER_SEC` | No | `60` | Token-bucket refill rate |
| `KRAB_RATE_LIMIT_FAIL_OPEN` | No | `true` in `dev`, `false` elsewhere | Whether to allow requests when the limiter backing store is unreachable |
| `KRAB_AUTH_FAILURE_WINDOW_SECS` | No | `60` | Length (seconds) of the fixed window for per-IP auth-failure tracking. Windows are tumbling, not sliding — the counter is keyed on `floor(unix_secs / window)` and resets at the boundary, so a client can spend up to `2 x KRAB_AUTH_FAILURE_THRESHOLD` failures across two adjacent windows. `0` and unparseable values fall back to `60` |
| `KRAB_AUTH_FAILURE_THRESHOLD` | No | `100` | Max auth failures per client IP allowed within the window before 429 is returned. `0` is valid and means lockdown: the first auth failure in the window is answered 429. Unparseable values fall back to `100` |
| `KRAB_TRUST_PROXY_HEADERS` | No | `false` | Trust `X-Forwarded-*` for client IP and protocol. **Only enable behind a proxy you control** |
| `KRAB_TRUSTED_PROXY_HOPS` | No | `1` | Only with `KRAB_TRUST_PROXY_HEADERS=true`: how many trusted proxy hops to skip from the right of `X-Forwarded-For` when choosing the client IP. `1` = rightmost entry. The candidate must parse as an IP or it is ignored |
| `KRAB_CORS_ORIGINS` | No | — | Comma-separated allowed origins. Unset means no cross-origin allowance |
| `KRAB_HTTP_REQUEST_TIMEOUT_SECS` | No | `30` | Per-request timeout applied innermost in the common HTTP stack; overruns return 408. `0` disables |
| `KRAB_HTTP_MAX_CONCURRENCY` | No | `1024` | Maximum concurrently processed requests. Excess requests queue (backpressure) and are bounded by the request timeout. `0` disables |
| `KRAB_HTTP_OVERLOAD_MODE` | No | `queue` | Concurrency overload strategy: `queue` (wait in queue bounded by timeout) or `shed` (fast-fail excess requests with 503 Service Unavailable). Trimmed and case-insensitive; an unrecognised value falls back to `queue` and logs `env_value_invalid_using_default` |

## Database

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `DATABASE_URL` | **Yes** for DB-backed services | — | **Secret.** Connection string |
| `KRAB_DB_DRIVER` | No | `postgres` | `postgres` \| `sqlite`. MySQL was removed deliberately and must not be reintroduced |
| `DB_MAX_CONNECTIONS` | No | `10` | Pool ceiling |
| `DB_MIN_CONNECTIONS` | No | `1` | Pool floor |
| `DB_ACQUIRE_TIMEOUT_SECS` | No | `5` | Wait before an acquire fails |
| `DB_IDLE_TIMEOUT_SECS` | No | `600` | Idle connection reaping |
| `DB_MAX_LIFETIME_SECS` | No | `1800` | Hard connection lifetime |
| `DB_CONNECT_RETRIES` | No | `5` | Startup connection attempts |
| `DB_CONNECT_RETRY_DELAY_MS` | No | `750` | Delay between startup attempts |

### Migration governance

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `DB_MIGRATION_ALLOW_APPLY` | **Yes** | — | `true` \| `false`. Whether this process may apply migrations |
| `DB_MIGRATION_FAILURE_POLICY` | **Yes** | — | `halt` \| `continue_non_critical` |
| `DB_MIGRATION_DRIFT_THRESHOLD` | No | `0` | Tolerated drift count before startup is blocked. `0` means any drift blocks |
| `DB_MIGRATION_RELEASE_ENVIRONMENTS` | No | `staging,prod` | Environments treated as release targets for promotion policy |
| `DB_MIGRATION_REQUIRE_REHEARSAL_IN_RELEASE` | No | `true` | Require rollback rehearsal evidence before applying in a release environment |

See [database reference](database.md) for the promotion policy and the
`rollback_sql` requirement.

### Compose-only Postgres bootstrap

Consumed by [`docker-compose.yml`](../../docker-compose.yml) and
[`docker/postgres/init/`](../../docker/postgres/init/), not by Rust code.

| Variable | Default | Purpose |
|---|---|---|
| `POSTGRES_USER` | — | Superuser created by the Postgres image |
| `POSTGRES_PASSWORD` | — | **Secret.** Superuser password |
| `POSTGRES_DB` | `krab` | Primary database |
| `POSTGRES_DB_USERS` | `krab_users` | Database created for `service_users` |

## Protocol selection and topology

Krab services can expose REST, GraphQL, and RPC from one codebase. These control
which are reachable and where they resolve. See
[protocol flexibility](../architecture/protocol_flexibility.md).

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_PROTOCOL_ENABLED` | No | `rest,graphql,rpc` | Comma-separated subset of `rest`, `graphql`, `rpc` |
| `KRAB_PROTOCOL_DEFAULT` | No | `rest` | Protocol chosen when the client expresses no preference |
| `KRAB_PROTOCOL_ENABLED_USERS` | No | — | Per-service override for `service_users`. When unset, `service_users` defaults to multi-exposure with GraphQL as default |
| `KRAB_PROTOCOL_EXPOSURE_MODE` | No | `single` | `single` \| `multi` |
| `KRAB_PROTOCOL_TOPOLOGY` | No | `single_service` | `single_service` \| split topology |
| `KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER` | No | `false` | Allow a request header to override protocol selection. **Leave off in production** |
| `KRAB_PROTOCOL_RESTRICTED_OPS_JSON` | No | — | JSON map of operation → allowed protocols, e.g. `{"auth.login":["rest"]}` |
| `KRAB_PROTOCOL_SPLIT_TARGETS_JSON` | No | — | JSON map of domain → per-protocol target URL |
| `KRAB_PROTOCOL_TENANT_OVERRIDES_JSON` | No | `{}` | JSON map of tenant → protocol override |
| `KRAB_PROTOCOL_TENANT_HINT_UNTRUSTED` | No | `false` | Opt-in: allow the unauthenticated `x-krab-tenant-id` header / `?tenant_id=` query to select tenant protocol policy when there is no authenticated tenant claim. **Dev only** — each use logs a warning. Left off, tenant policy keys solely off the JWT claim |
| `KRAB_PROTOCOL_EXTERNAL_MODE` | No | `false` | Treat protocol exposure as externally reachable |
| `KRAB_PROTOCOL_GATEWAY_BASE_URL` | No | — | Gateway origin when `KRAB_PROTOCOL_EXTERNAL_MODE` is on |
| `KRAB_RUNTIME_TOPOLOGY` | No | — | Overrides the service-contract topology mode at runtime |
| `KRAB_RUNTIME_ENDPOINTS_JSON` | No | — | JSON map of service name → endpoint. Overrides `KRAB_AUTH_BASE_URL` and `KRAB_USERS_BASE_URL` |
| `KRAB_SERVER_FN_TIMEOUT_MS` | No | `30000` | Timeout in milliseconds for native (non-WASM) server-function RPC calls. Read once at first call; `0` or a non-numeric value falls back to the default |

## Frontend rendering and caching

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_HYDRATION_MODE` | No | `wasm` | `wasm` \| `minimal_js` \| `ssr_only`. An unrecognised value logs `hydration_mode_invalid_fallback` (`KRAB-HYDRATE-001`) and falls back to `wasm` |
| `KRAB_MINIMAL_JS_AUDIT` | No | `true` | Emit the minimal-JS audit trail during render |
| `KRAB_ISR_REVALIDATE_SECS` | No | `30` | ISR revalidation window, minimum `1` |
| `KRAB_SSR_STREAM_BUDGET_BYTES` | No | `2097152` (2 MiB) | Streaming render byte budget, minimum `1024` |
| `KRAB_CACHE_NAMESPACE` | No | `default` | Cache key namespace. Change to isolate deployments sharing a Redis instance |
| `KRAB_CACHE_MAX_BODY_BYTES` | No | `10485760` (10 MiB) | Largest cacheable response body, minimum `1024` |
| `KRAB_DISTRIBUTED_CACHE_TTL_SECS` | No | `60` | Distributed cache TTL, clamped to `1`–`3600` |
| `KRAB_REDIS_URL` | Conditional | — | Required by the `redis-store` feature, the distributed cache, **and the ISR cache**. Without it, ISR falls back to a per-process store — see below. If set while the binary was built without `redis-store`, startup fails closed outside `dev` |
| `FRONTEND_ISR_QUERY_ALLOWLIST` | No | — (path-only) | `service_frontend`: comma-separated query-parameter names allowed into ISR/SWR cache keys. Empty means cache keys are path-only, so arbitrary query strings cannot multiply cache entries |
| `KRAB_MEMORY_STORE_MAX_ENTRIES` | No | `100000` | Max live entries in an in-process `MemoryStore` (read once per process; `0` = unlimited). At capacity, expired entries are reclaimed first, then oldest-inserted are evicted with a throttled `memory_store_evicted` warning |
| `KRAB_WS_MAX_ROOMS` | No | `0` (unlimited) | Maximum WebSocket rooms a `WsRoomManager` will create. Read once at construction; at the cap, `try_room()` returns an error and the infallible `room()` logs `ws_room_cap_reached` and returns a detached room. `reap_empty()` frees slots |

## Build and tooling

| Variable | Read by | Default | Purpose |
|---|---|---|---|
| `KRAB_SSG_BLOG_SLUGS` | `service_frontend/build.rs` | — | Comma-separated extra slugs to pre-render at build time |
| `KRAB_REQUIRE_WASM_OPT` | `krab doctor` | unset (advisory) | When truthy, a missing `wasm-opt` becomes a hard failure instead of a warning |
| `KRAB_NFT` | NFT scripts and compose | `0` | Marks a non-functional-test run |

## Test-only tuning

These are read inside `#[test]` code paths only. They tune assertion thresholds
for the streaming SSR profile suite and have no effect on a running service.

| Variable | Default |
|---|---|
| `KRAB_REQUIRE_DB_TESTS` | unset. The `krab_core` DB test suite (migration lifecycle, drift, rollback, governance) needs a reachable Postgres (`DATABASE_URL`, default `postgres://postgres@localhost:5432/krab_test`). When the database is unreachable the tests print a loud `SKIPPED` line on stderr and return. Set `1`/`true` (CI mode) to make an unreachable database **panic** the suite instead of skipping — use this wherever those tests are a gate |
| `KRAB_SSR_STREAM_SLO_P95_MS` | `200` |
| `KRAB_SSR_STREAM_SLO_P99_MS` | `400` |
| `KRAB_SSR_STREAM_FAST_P95_MS` | `200` |
| `KRAB_SSR_STREAM_SLOW_P95_MS` | `400` |
| `KRAB_SSR_MIXED_PROFILE_SAMPLES` | `1200`, minimum `100` |
| `KRAB_SSR_MIXED_PROFILE_SLOW_EVERY` | `5`, minimum `2` |
| `KRAB_SSR_MIXED_PROFILE_SLOW_PENALTY_MS` | `5` |

---

## Local stack bootstrap

```bash
cargo run -p krab_cli -- bootstrap
```

Builds the workspace and starts every service declared in
[`krab.toml`](../../krab.toml) under the orchestrator, for deterministic
onboarding.

## Adding a variable

A new configuration knob is not complete until it appears in **all three**
places, in the same change:

1. [`.env.example`](../../.env.example) — with a representative value
2. This page — with its default and accepted values
3. [`CHANGELOG.md`](../../CHANGELOG.md) — under `[Unreleased]`

Secrets must be read through `krab_core::config::read_env_or_file()`, never
`std::env::var` directly.
