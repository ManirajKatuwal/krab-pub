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

Strict mode fails on warnings as well as errors. `krab env-check` and
`krab doctor` load `./.env` before evaluating the policy; a variable already set
in the process environment wins, and the loaded file and variable count are
reported on stderr. The services themselves do **not** read `.env` — export the
variables (or use your process manager's env file) when running them.

---

## Reading conventions

- **Required** means startup fails without it.
- **Default** is what the runtime uses when the variable is absent or unparsable.
  Where a value is clamped, the bound is given.
- Boolean variables are true only for `1` or `true` (case-insensitive). **Any
  other value — including `yes` and `on` — reads as false.** The exceptions are
  the two booleans `service_frontend` parses itself, `KRAB_MINIMAL_JS_AUDIT` and
  (in its protocol client) `KRAB_PROTOCOL_EXTERNAL_MODE`, which also accept
  `yes`/`on` and `no`/`off`.
- List variables are comma-separated unless noted.

### Secret sourcing

Secrets are read through `krab_core::config::read_env_or_file()`, which resolves
three forms in order:

| Form | Example | Notes |
|---|---|---|
| Inline | `KRAB_JWT_SECRET=…` | **Rejected at startup in `prod`** (and any unrecognised `KRAB_ENVIRONMENT`); a warning in `staging` |
| File | `KRAB_JWT_SECRET_FILE=/run/secrets/jwt` | Contents must be non-empty. The form to use outside `dev` |
| Vault reference | `KRAB_JWT_SECRET_VAULT_REF=kv/data/krab/auth#jwt_secret` | **Always an error today.** There is no runtime vault resolver: `read_env_or_file()` refuses a `_VAULT_REF`, and in `staging`/`prod` the startup policy rejects it too. Materialise the secret to a file and use `_FILE` |

Any variable marked **secret** below is read this way, so its `_FILE` form works.
The startup secrets policy (`KrabConfig::validate_all()`, which every reference
service calls) checks `DATABASE_URL`, `KRAB_REDIS_URL`, `KRAB_SMTP_PASSWORD`,
`KRAB_JWT_SECRET`, `KRAB_JWT_KEYS_JSON`, `KRAB_JWT_PROVIDERS_JSON` and
`KRAB_BEARER_TOKEN`: in `prod` an inline value without a `_FILE` is a startup
error, and outside `dev` `KRAB_BEARER_TOKEN` must be unset. (`KRAB_SMTP_PASSWORD`
is on that list but nothing in Krab reads it.) See
[security reference](security.md).

The suffixed forms are not listed individually in the tables below — every
variable marked **secret** has them.

---

## Core service

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_ENVIRONMENT` | Recommended | `dev` | `dev` \| `staging` \| `prod`, case-insensitive; `production` is an alias of `prod` and **`local` is an alias of `dev`** (dev rules everywhere: `krab_core`, `krab env-check`/`krab doctor`, `service_auth`'s password check; the migration promotion ladder keeps `local` as the stage below `dev`). Gates secret-sourcing enforcement, static-auth and CORS validation, and migration promotion policy. **Any other value is an unrecognised environment that `krab_core` validates as strictly as `prod`** (explicit `KRAB_CORS_ORIGINS`, no static auth, no inline secrets), so a typo fails closed. (Before 0.6.0 `krab_core` parsed `local` as unrecognised and applied prod rules to it, while the CLI, `service_auth` and the ladder treated it as dev.) |
| `KRAB_SERVICE_NAME` | No | per service | **Per-service.** Service identity used in telemetry (the `service` field on every log line and metric), migration records, and the `KRAB_PROTOCOL_ENABLED_<NAME>` lookup. The default is whatever the binary passes to `KrabConfig::from_env_checked` — `frontend`, `auth`, `users`, `users-split` for the reference services, not `krab`. Setting it in a shared environment renames **every** service that inherits it, and it degrades silently: nothing fails, they simply all report one name. Under `krab bootstrap` the orchestrator injects each service's own value from `[services.X].service_name` in `krab.toml` |
| `KRAB_SERVICE` | No | `service` | Fallback service identity for protocol selection when `KRAB_SERVICE_NAME` is unset |
| `KRAB_HOST` | No | `127.0.0.1` | Bind address |
| `KRAB_PORT` | No | per service | **Per-service.** Bind port. The per-service default (`3000` frontend, `3001` auth, `3002` users, `3207` users-split) is a *fallback*, not a floor: when `KRAB_PORT` is set it overrides all of them simultaneously, so every service that inherits it tries to bind the same port. Under `krab bootstrap` the orchestrator injects each service's own value from `[services.X].port` in `krab.toml`, which is what keeps a service on the port its health probe addresses |
| `KRAB_PUBLIC_BASE_URL` | No | `http://localhost:3000` | Public origin for SEO metadata (`canonical`, `og:url`) and `/robots.txt`, `/sitemap.xml`. **Set explicitly to your external HTTPS URL in staging and prod** |
| `RUST_LOG` | No | `info` | `tracing-subscriber` env filter, e.g. `info,krab_core=debug` |

## Authentication

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_AUTH_MODE` | Recommended | `jwt` | `jwt` \| `oidc` (a synonym for `jwt`) \| `static`. `static` is for `dev` only — startup rejects it in any other environment |
| `KRAB_BEARER_TOKEN` | Conditional | — | **Secret.** Shared token when `KRAB_AUTH_MODE=static`. `_FILE` / `_VAULT_REF` sourcing works as for every secret (it was silently ignored before 0.6.0) |
| `KRAB_AUTH_PUBLIC_PATHS` | No | — | Comma-separated paths exempt from auth, **in addition to** the open-path list. Trailing `*` is a prefix match. Everything else is deny-by-default on protected routers |
| `KRAB_AUTH_OPEN_PATHS` | No | built-in list | Comma-separated patterns **replacing** the built-in open (no-auth) path list: `/health`, `/ready`, plus — in 0.6.x only — the deprecated application routes below. Trailing `*` is a prefix match. Set it to close default-open paths; an explicitly empty value closes them all. When set (even empty) it is the **complete** open list: paths a service declares in code (`RuntimeState::with_public_paths`) are ignored, with a warning. Unset keeps the built-in defaults. Does **not** govern the metrics endpoints — see `KRAB_METRICS_PUBLIC` |
| `KRAB_AUTH_LEGACY_OPEN_PATHS` | No | `true` | Whether the default open-path list still includes the application routes it shipped with (`/`, `/contact`, `/api/contact`, `/api/status`, `/data/dashboard`, `/rpc/version`, `/rpc/now`, `/asset-manifest.json`, `/blog/*`, `/pkg/*`, and the `/api/v1/auth/*` token endpoints). **Deprecated in 0.6.0**; the entries leave the default in 0.7.0 and a startup warning names them until then. Declare your service's public routes with `KRAB_AUTH_PUBLIC_PATHS` or `RuntimeState::with_public_paths`, then set `false`. Ignored when `KRAB_AUTH_OPEN_PATHS` is set |
| `KRAB_METRICS_PUBLIC` | No | `false` | Allow anonymous `GET /metrics` and `/metrics/prometheus`. **Off by default:** metrics publish route names, traffic shape, error rates, and latency distributions. Prefer authenticating your scraper or binding metrics to a network only it can reach; set `true` only when that surface is deliberately public. Applies on top of `KRAB_AUTH_OPEN_PATHS`, so it reopens metrics whether or not that list is set |
| `KRAB_AUTH_ADMIN_ROLE` | No | `admin` | Role name granting admin routes |
| `KRAB_AUTH_ADMIN_SCOPE` | No | `admin` | Scope name granting admin routes |
| `KRAB_AUTH_REQUIRED_ROLES` | No | — | Comma-separated roles required on protected routes |
| `KRAB_AUTH_REQUIRED_SCOPES` | No | — | Comma-separated scopes required on protected routes |
| `KRAB_AUTH_REQUIRED_CLAIMS_JSON` | No | — | JSON object of claim/value pairs every token must carry |
| `KRAB_AUTH_ROUTE_POLICIES_JSON` | No | — | JSON map of route pattern → policy, overriding the baseline policy per route |
| `KRAB_AUTH_REQUIRE_TENANT_CLAIM` | No | `false` | Reject any token that carries no tenant claim with `401`. Independent of `KRAB_AUTH_REQUIRE_TENANT_MATCH` |
| `KRAB_AUTH_REQUIRE_TENANT_MATCH` | No | `true` | On a path containing `/tenants/<id>/`, require the token's tenant claim to equal `<id>` (401 otherwise). Paths without a `tenants` segment are unaffected. Set `false` to disable |
| `KRAB_AUTH_COOKIE_SESSION_ENABLED` | No | `false` | Enable cookie-backed sessions. **Implies CSRF protection** — enabling this turns on the same protection as `KRAB_CSRF_ENABLED` |
| `KRAB_CSRF_ENABLED` | No | `false` | Enforce CSRF protection on unsafe methods (`POST`, `PUT`, `PATCH`, `DELETE`). Enabled automatically when `KRAB_AUTH_COOKIE_SESSION_ENABLED` is on |
| `KRAB_AUTH_ACCESS_TTL_SECS` | No | service default | Access-token lifetime issued by `service_auth` |
| `KRAB_AUTH_REFRESH_TTL_SECS` | No | service default | Refresh-token lifetime issued by `service_auth` |
| `KRAB_AUTH_BOOTSTRAP_USER` | No | — | Bootstrap account username for `service_auth` |
| `KRAB_AUTH_BOOTSTRAP_PASSWORD` | No | — | **Secret.** Bootstrap account password, as an Argon2id PHC hash. Plaintext is accepted in `dev`/`local` only and hashed at startup |
| `KRAB_AUTH_LOGIN_USERS_JSON` | No | — | **Secret.** JSON object of `username` → Argon2id PHC hash. Generate entries with `krab auth hash-password --username <name>` |
| `KRAB_SERVICE_AUTH_SCOPE` | No | `service:internal` | Scope required for service-to-service calls |
| `KRAB_FRONTEND_DOWNSTREAM_BEARER_TOKEN` | No | — | **Secret.** Downstream bearer token for frontend to authenticate calls to backend services |
| `KRAB_FRONTEND_PKG_DIR` | No | `dist/pkg` | Directory holding the built `service_frontend_islands.js`. The frontend hashes that file to publish a real `integrity` digest and `?h=` cache buster in `/asset-manifest.json`; when it cannot be read, no integrity is published and the browser treats hydration as degraded |
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
| `KRAB_JWT_PROVIDERS_JSON` | No | — | **Secret.** JSON array of providers, each with `name`, `issuer`, `audience`, and `keys` (`kid` → key material) and/or `jwks_url`; optional `required_claims` and `key_not_after` (`kid` → retirement time) |
| `KRAB_OIDC_JWKS_URL` | No | — | The default provider's published JSON Web Key Set. Keys are fetched and cached off the request path, refreshed in the background, and refetched when a token names an unknown `kid`. With it, `KRAB_JWT_SECRET` / `KRAB_JWT_KEYS_JSON` are optional. **`https://` required outside dev** (startup and the verifier both refuse plain HTTP). Set `KRAB_JWT_ALLOWED_ALGS` to the provider's algorithm (e.g. `RS256`) — the default allowlist is `HS256` only. Until the first successful fetch, requests needing it get 503 |
| `KRAB_OIDC_JWKS_REFRESH_SECS` | No | `300` | Background refresh interval for every JWKS. Minimum `30`. Without a Tokio runtime there is no background task: the first request after the interval refreshes the set, and a warning is logged |
| `KRAB_OIDC_JWKS_MIN_REFETCH_SECS` | No | `30` | Minimum gap between on-demand refetches triggered by an unknown `kid`, per key set; floored at `1`. Measured from both the start and the end of the last attempt, and refetches are single-flight. Stops a stream of made-up `kid`s from turning the service into a request amplifier against the identity provider |
| `KRAB_OIDC_JWKS_TIMEOUT_MS` | No | `3000` | Timeout for one JWKS fetch, minimum `100` |
| `KRAB_JWT_KEY_NOT_AFTER_JSON` | No | — | JSON object `kid` → time after which that key no longer verifies, as RFC 3339 (`"2026-10-01T00:00:00Z"`) or Unix seconds. Retires a rotated-out key on a schedule. An unparseable value fails closed (every request 503) rather than meaning "never" |
| `KRAB_JWT_ACTIVE_KID` | No | — | KID used for newly issued tokens (by `service_auth`). Older KIDs keep verifying until removed from the key set or until their `KRAB_JWT_KEY_NOT_AFTER_JSON` time |
| `KRAB_JWT_REQUIRE_KID` | No | `false` | Reject tokens with no `kid` header |
| `KRAB_JWT_ALLOWED_ALGS` | No | `HS256` | Comma-separated allow-list, e.g. `RS256,ES256`. Mixing HMAC (`HS*`) with asymmetric (`RS*`/`PS*`/`ES*`/`EdDSA`) families is rejected — startup fails outside dev, and the request path refuses to verify (503) in every environment. Outside dev, startup also fails when a JWKS URL is configured and this list is HMAC-only (including the `HS256` default) |
| `KRAB_JWT_LEEWAY_SECS` | No | `30` | Clock-skew tolerance on `exp` and `nbf`, in seconds. (Documented as "unset means no leeway" until 0.6.0; the code has always defaulted to 30) |

## Rate limiting, CORS, and proxy trust

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_RATE_LIMIT_CAPACITY` | No | `120` | Global per-IP limiter: requests allowed per client IP per window. The limiter is a fixed-window counter, not a token bucket — a burst straddling a window boundary can reach twice this |
| `KRAB_RATE_LIMIT_REFILL_PER_SEC` | No | `60` | Sets the global limiter's window length: `ceil(capacity / refill)` seconds, clamped to 1–300 (2 s at the defaults) |
| `KRAB_RATE_LIMIT_FAIL_OPEN` | No | `true` in `dev`, `false` elsewhere | Whether to allow requests when the limiter backing store is unreachable |
| `KRAB_AUTH_FAILURE_WINDOW_SECS` | No | `60` | Length (seconds) of the fixed window for per-IP auth-failure tracking. Windows are tumbling, not sliding — the counter is keyed on `floor(unix_secs / window)` and resets at the boundary, so a client can spend up to `2 x KRAB_AUTH_FAILURE_THRESHOLD` failures across two adjacent windows. `0` and unparseable values fall back to `60` |
| `KRAB_AUTH_FAILURE_THRESHOLD` | No | `100` | Max auth failures per client IP allowed within the window before 429 is returned. `0` is valid and means lockdown: the first auth failure in the window is answered 429. Unparseable values fall back to `100`. Only 401-class failures count (a provider outage does not). A token **without** a `kid` from an address over the threshold is answered 429 before verification; tokens naming a `kid` are always verified |
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
| `DB_MIGRATION_ALLOW_APPLY` | No | `true` | `true` \| `false`. Whether this process may apply migrations. Set it explicitly per environment |
| `DB_MIGRATION_FAILURE_POLICY` | No | `halt` | `halt` \| `continue_non_critical` (case-insensitive). Any other value means `halt` |
| `DB_MIGRATION_DRIFT_THRESHOLD` | No | `0` | Read into `MigrationGovernanceConfig::drift_tolerance_threshold`: the tolerated count of unexpected versions a caller passes to `enforce_drift_policy`. Not applied automatically — nothing blocks startup on it unless the service calls that function |
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
| `KRAB_USERS_REST_SERVICE_NAME`, `KRAB_USERS_GRAPHQL_SERVICE_NAME`, `KRAB_USERS_RPC_SERVICE_NAME` | No | `users-rest` / `users-graphql` / `users-rpc` | Service identity of `service_users`'s single-protocol binaries (`users-rest`, `users-graphql`, `users-rpc`). Each binary copies its value into `KRAB_SERVICE_NAME` at startup, and also sets `KRAB_PROTOCOL_EXPOSURE_MODE=single`, `KRAB_PROTOCOL_ENABLED`/`KRAB_PROTOCOL_DEFAULT` to its protocol and `KRAB_PROTOCOL_TOPOLOGY=split_services`, overriding any inherited values |
| `KRAB_USERS_REST_HOST`, `KRAB_USERS_GRAPHQL_HOST`, `KRAB_USERS_RPC_HOST` | No | inherits `KRAB_HOST` | Bind address for the matching single-protocol binary; copied into `KRAB_HOST` when set |
| `KRAB_USERS_REST_PORT`, `KRAB_USERS_GRAPHQL_PORT`, `KRAB_USERS_RPC_PORT` | No | `3101` / `3102` / `3103` | Bind port for the matching single-protocol binary; always copied into `KRAB_PORT`, so an inherited `KRAB_PORT` does not apply to these binaries |
| `KRAB_SERVER_FN_TIMEOUT_MS` | No | `30000` | Timeout in milliseconds for native (non-WASM) server-function RPC calls. Read once at first call; `0` or a non-numeric value falls back to the default |

## Frontend rendering and caching

| Variable | Required | Default | Accepts |
|---|---|---|---|
| `KRAB_HYDRATION_MODE` | No | `wasm` | `wasm` \| `minimal_js` \| `ssr_only`. An unrecognised value logs `hydration_mode_invalid_fallback` (`KRAB-HYDRATE-001`) and falls back to `wasm` |
| `KRAB_MINIMAL_JS_AUDIT` | No | `true` | Emit the minimal-JS audit trail during render |
| `KRAB_HYDRATION_BUDGET_HOME_STARTUP_MS` | No | `1500` | `service_frontend`: hydration-time budget (ms) for `/`, logged with `hydration_mode_selected` and passed to `/_krab/home.js` as `routeBudgets.hydrationMs` |
| `KRAB_HYDRATION_BUDGET_HOME_TTFB_MS` | No | `800` | `service_frontend`: TTFB budget (ms) for `/`, logged with `hydration_mode_selected` and passed to `/_krab/home.js` as `routeBudgets.ttfbMs` |
| `KRAB_HYDRATION_BUDGET_HOME_JS_KB` | No | `160` | `service_frontend`: JS budget (KB) for `/`. Read into the route budget but not currently enforced or published |
| `KRAB_HYDRATION_BUDGET_HOME_WASM_KB` | No | `512` | `service_frontend`: WASM budget (KB) for `/`. Read into the route budget but not currently enforced or published |
| `KRAB_ISR_REVALIDATE_SECS` | No | `30` | ISR revalidation window, minimum `1` |
| `KRAB_SSR_STREAM_BUDGET_BYTES` | No | `2097152` (2 MiB) | Streaming render byte budget, minimum `1024` |
| `KRAB_CACHE_NAMESPACE` | No | `default` | Cache key namespace. Change to isolate deployments sharing a Redis instance |
| `KRAB_CACHE_MAX_BODY_BYTES` | No | `10485760` (10 MiB) | Largest cacheable response body, minimum `1024` |
| `KRAB_DISTRIBUTED_CACHE_TTL_SECS` | No | `60` | Distributed cache TTL, clamped to `1`–`3600` |
| `KRAB_REDIS_URL` | Conditional | — | Required by the `redis-store` feature, the distributed cache, **and the ISR cache**. Without it, ISR falls back to a per-process store — see below. If set while the binary was built without `redis-store`, startup fails closed outside `dev`. It is a policy-checked secret (it can carry a password): an inline value is rejected in `prod`, so set `KRAB_REDIS_URL_FILE` there. The store reads it through `read_env_or_file` (inline, then `_FILE`, then `_VAULT_REF`); a source that is set but unreadable — a missing file, an empty file, an unresolved vault ref — fails startup outside `dev` exactly like a malformed URL. (Before 0.6.0 the store read only the inline variable, so `KRAB_REDIS_URL_FILE` passed the policy and was then ignored, leaving the service on the per-process store.) |
| `FRONTEND_ISR_QUERY_ALLOWLIST` | No | — (path-only) | `service_frontend`: comma-separated query-parameter names allowed into ISR/SWR cache keys. Empty means cache keys are path-only, so arbitrary query strings cannot multiply cache entries |
| `KRAB_MEMORY_STORE_MAX_ENTRIES` | No | `100000` | Max live entries in an in-process `MemoryStore` (read once per process; `0` = unlimited). At capacity, expired entries are reclaimed first, then oldest-inserted are evicted with a throttled `memory_store_evicted` warning |
| `KRAB_WS_MAX_ROOMS` | No | `0` (unlimited) | Maximum WebSocket rooms a `WsRoomManager` will create. Read once at construction; at the cap, `try_room()` returns an error and the infallible `room()` logs `ws_room_cap_reached` and returns a detached room. `reap_empty()` frees slots |

## Build and tooling

| Variable | Read by | Default | Purpose |
|---|---|---|---|
| `KRAB_ARTIFACT_DIR` | `krab_cli`, `krab_orchestrator` | `.krab` | Root directory for generated artifacts written by the tooling: `krab db rehearsal` evidence (`<root>/evidence/rollback-rehearsal-evidence.txt`) and `krab release certify` bundles (`<root>/release-certify/local`) when `--out` is omitted, and `krab_orchestrator` service logs (`<root>/orchestrator/run-<ts>-<pid>/`). Empty counts as unset. When unset and an `internal/audit/` directory exists in the working directory, that directory is used with a deprecation warning; this fallback is removed in 0.7.0. Relative paths resolve against the working directory |
| `KRAB_SSG_BLOG_SLUGS` | `service_frontend/build.rs` | — | Comma-separated extra slugs to pre-render at build time |
| `KRAB_REQUIRE_WASM_OPT` | `krab doctor` | unset (advisory) | When truthy, a missing `wasm-opt` becomes a hard failure instead of a warning |
| `KRAB_NFT` | NFT scripts and compose | `0` | Marks a non-functional-test run |

## Test-only tuning

These are read inside `#[test]` code paths only. They select test backends and tune assertion thresholds
for the streaming SSR profile suite and have no effect on a running service.

| Variable | Default |
|---|---|
| `KRAB_TEST_REDIS_URL` | unset. The Redis server the `krab_core` `RedisStore` tests run against (`--features redis-store`), e.g. `redis://127.0.0.1:6379/15`. Unset or unreachable: each test prints a loud `SKIPPED` line and returns |
| `KRAB_REQUIRE_REDIS_TESTS` | unset. `1` makes a missing or unreachable `KRAB_TEST_REDIS_URL` fail the Redis tests instead of skipping them — use it wherever they are a gate |
| `KRAB_REQUIRE_DB_TESTS` | unset. The `krab_core` DB test suite (migration lifecycle, drift, rollback, governance) needs a reachable Postgres (`KRAB_TEST_DATABASE_URL`, then `DATABASE_URL`, default `postgres://postgres@localhost:5432/krab_test`). When the database is unreachable the tests print a loud `SKIPPED` line on stderr and return. Set `1`/`true` (CI mode) to make an unreachable database **panic** the suite instead of skipping — use this wherever those tests are a gate |
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
