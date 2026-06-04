# Security Architecture

This document covers Krab's security architecture, secret management model, authentication system, and threat mitigations.

## Security Principles

1. **Deny by default**: All API routes require authentication unless explicitly marked public.
2. **Fail closed**: Missing or invalid configuration causes startup failure — never silent fallback.
3. **Secrets never inline**: Production environments reject inline secrets; only file-mounted or vault-sourced secrets are accepted.
4. **Zero advisory tolerance**: `cargo-deny` enforces zero ignored advisories in CI.
5. **Least privilege**: Services run with minimal database credentials; superuser access is logged and warned against in production.

---

## Authentication Model

Krab supports two authentication modes, controlled by `KRAB_AUTH_MODE`:

### JWT / OIDC Mode (production)

```
KRAB_AUTH_MODE=jwt    # or oidc
```

- **Token issuance**: `service_auth` issues HS256-signed JWT access + refresh token pairs.
- **Key rotation**: `KeyRing` supports multiple signing keys (`kid`). The active key is selected via `KRAB_JWT_ACTIVE_KID`.
- **Validation**: All services validate tokens against issuer (`KRAB_OIDC_ISSUER`) and audience (`KRAB_OIDC_AUDIENCE`) with clock-skew leeway.
- **Algorithm allowlist**: Verification only accepts algorithms listed in `KRAB_JWT_ALLOWED_ALGS` (CSV). Defaults to `HS256` when unset.
- **Secret sourcing**: Runtime JWT validation reads key material through the shared `read_env_or_file` path, so `*_FILE` secret mounts work end-to-end.
- **Refresh**: Single-use refresh tokens with replay detection (store-backed).
- **Revocation**: Token revocation is enforced via runtime store lookups, and refresh tokens are rejected when presented to protected application routes.
- **JWKS**: Key descriptors exposed at `/api/v1/auth/jwks`.

### Static Mode (development only)

```
KRAB_AUTH_MODE=static
```

- A single bearer token (`KRAB_BEARER_TOKEN`) is accepted.
- **Blocked in non-dev environments** — startup fails with a clear error message.

---

## Secret Management

### The `read_env_or_file` Pattern

All sensitive configuration in Krab follows the `read_env_or_file` pattern (defined in `krab_core::config`):

1. First, check for the standard environment variable (e.g., `KRAB_JWT_SECRET`).
2. If not found, check for the `*_FILE` variant (e.g., `KRAB_JWT_SECRET_FILE`).
3. If the `*_FILE` variant is set, read the secret from the specified file path.
4. If neither is set, return `None` (the caller decides the fallback behavior).

This pattern supports:

- **Docker/Kubernetes secrets**: Mount secrets as files at `/run/secrets/` and reference via `*_FILE`.
- **CI/CD pipelines**: Set secrets as environment variables directly.
- **Vault integration**: Use `*_VAULT_REF` variables for external vault resolution.

### Production Enforcement

When `KRAB_ENVIRONMENT` is `staging`, `prod`, or any non-dev value:

| Rule                                  | Enforcement                                                                                |
| ------------------------------------- | ------------------------------------------------------------------------------------------ |
| Inline JWT secrets forbidden          | Startup fails if `KRAB_JWT_SECRET` is set without `*_FILE` or `*_VAULT_REF`                |
| Insecure default secrets rejected     | Startup fails if secrets match known defaults (e.g., `krab-insecure-dev-secret-change-me`) |
| Static auth mode blocked              | Startup fails if `KRAB_AUTH_MODE=static`                                                   |
| Static bearer tokens blocked          | Startup fails if `KRAB_BEARER_TOKEN` is set in JWT/OIDC mode                               |
| Database default credentials rejected | Startup fails if `DATABASE_URL` contains default password patterns                         |

### Secret variables reference

| Secret                | Env Var                        | File Var                            | Vault Var                                |
| --------------------- | ------------------------------ | ----------------------------------- | ---------------------------------------- |
| JWT signing secret    | `KRAB_JWT_SECRET`              | `KRAB_JWT_SECRET_FILE`              | `KRAB_JWT_SECRET_VAULT_REF`              |
| JWT key ring (JSON)   | `KRAB_JWT_KEYS_JSON`           | `KRAB_JWT_KEYS_JSON_FILE`           | `KRAB_JWT_KEYS_JSON_VAULT_REF`           |
| JWT providers (JSON)  | `KRAB_JWT_PROVIDERS_JSON`      | `KRAB_JWT_PROVIDERS_JSON_FILE`      | `KRAB_JWT_PROVIDERS_JSON_VAULT_REF`      |
| Bootstrap password    | `KRAB_AUTH_BOOTSTRAP_PASSWORD` | `KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE` | `KRAB_AUTH_BOOTSTRAP_PASSWORD_VAULT_REF` |
| Login user map (JSON) | `KRAB_AUTH_LOGIN_USERS_JSON`   | `KRAB_AUTH_LOGIN_USERS_JSON_FILE`   | `KRAB_AUTH_LOGIN_USERS_JSON_VAULT_REF`   |
| Database URL          | `DATABASE_URL`                 | `DATABASE_URL_FILE`                 | —                                        |

---

## Rate Limiting

Rate limiting is applied globally via `krab_core::http` middleware:

| Variable                         | Description                                        | Default                            |
| -------------------------------- | -------------------------------------------------- | ---------------------------------- |
| `KRAB_RATE_LIMIT_CAPACITY`       | Maximum burst capacity                             | `120`                              |
| `KRAB_RATE_LIMIT_REFILL_PER_SEC` | Token refill rate per second                       | `60`                               |
| `KRAB_RATE_LIMIT_FAIL_OPEN`      | Store-failure policy (`true`=allow, `false`=block) | `true` in `dev`, `false` otherwise |

When the limit is exceeded, the service returns `HTTP 429 Too Many Requests`.

If the distributed store is unavailable, policy is controlled by `KRAB_RATE_LIMIT_FAIL_OPEN`:

- `true` (fail-open): request is allowed and a warning is emitted.
- `false` (fail-closed): request is denied (`429`) and a warning is emitted.

---

## CORS

CORS origins are configured via `KRAB_CORS_ORIGINS` (comma-separated).

- In `staging`/`prod`, startup fails if `KRAB_CORS_ORIGINS` is empty.
- In `dev`, wildcard fallback is allowed only when no explicit list is provided.

```sh
KRAB_CORS_ORIGINS="https://app.example.com,https://admin.example.com"
```

## Trusted Proxy Headers

Client IP extraction defaults to **not trusting** proxy headers.

| Variable                   | Description                                         | Default |
| -------------------------- | --------------------------------------------------- | ------- |
| `KRAB_TRUST_PROXY_HEADERS` | Trust `x-forwarded-for` / `x-real-ip` for client IP | `false` |

When disabled, middleware ignores forwarded headers and falls back to the TCP peer address when available; otherwise it uses an `unknown` fallback identity for per-IP controls.

### Current proxy-header trust semantics

- [`extract_client_ip()`](../crates/framework/krab_core/src/http.rs:590) only consults `x-forwarded-for` and `x-real-ip` when [`KRAB_TRUST_PROXY_HEADERS`](../crates/framework/krab_core/src/config.rs:253) is enabled.
- When enabled, [`extract_client_ip()`](../crates/framework/krab_core/src/http.rs:590) takes the left-most `x-forwarded-for` value if present, otherwise `x-real-ip`.
- When proxy headers are disabled or absent, [`extract_client_ip()`](../crates/framework/krab_core/src/http.rs:590) now falls back to the socket peer address when Axum connect-info is available, and only then to `unknown`.
- Krab does **not** currently implement a trusted proxy CIDR allowlist or hop-count validation in [`extract_client_ip()`](../crates/framework/krab_core/src/http.rs:590). This means `KRAB_TRUST_PROXY_HEADERS=true` should only be enabled behind an ingress or reverse proxy that strips and rewrites those headers.

---

## CSRF Protection

CSRF protection in Krab is currently **opt-in**, not universally enforced.

### Enforcement conditions

- CSRF checks are enabled only when [`csrf_protection_enabled()`](../crates/framework/krab_core/src/http.rs:722) returns true.
- That currently happens when either `KRAB_CSRF_ENABLED=true` or `KRAB_AUTH_COOKIE_SESSION_ENABLED=true`.
- [`csrf_protection_middleware()`](../crates/framework/krab_core/src/http.rs:794) only enforces checks for unsafe HTTP methods (`POST`, `PUT`, `PATCH`, `DELETE`) and only when the request carries a `cookie` header.
- Requests without cookies are treated as non-browser or token-style requests and bypass CSRF validation in [`csrf_protection_middleware()`](../crates/framework/krab_core/src/http.rs:810).

### Token model

- [`csrf_token_endpoint()`](../crates/framework/krab_core/src/http.rs:752) issues a double-submit token and sets a `krab_csrf_token` cookie with `SameSite=Strict; HttpOnly; Secure; Path=/`.
- The middleware compares the cookie token from [`csrf_cookie_token()`](../crates/framework/krab_core/src/http.rs:733) with the header token from [`csrf_header_token()`](../crates/framework/krab_core/src/http.rs:741) using constant-time comparison in [`csrf_protection_middleware()`](../crates/framework/krab_core/src/http.rs:818).

### Current limitation

- The WASM server-function client helper in [`call_server_fn()`](../crates/framework/krab_core/src/server_fn.rs:230) looks for a cookie named `csrf_token`, but the HTTP middleware currently issues and validates `krab_csrf_token` in [`csrf_token_endpoint()`](../crates/framework/krab_core/src/http.rs:752).
- As a result, CSRF propagation for server functions is **not aligned by default** with the middleware-issued token name and should not be described as guaranteed protection until the names or integration are unified.

## Browser security headers and CSP

- Krab sets security headers in the shared HTTP layer in [`security_headers_middleware()`](../crates/framework/krab_core/src/http.rs:680).
- The current Content Security Policy is:

```text
default-src 'self'; script-src 'self' 'wasm-unsafe-eval'
```

- The `'wasm-unsafe-eval'` directive is presently enabled in [`crates/framework/krab_core/src/http.rs`](../crates/framework/krab_core/src/http.rs:693) to support the current WebAssembly execution model.
- This is a deliberate compatibility tradeoff, not a maximally strict CSP posture. Deployments with stricter browser isolation requirements should review whether their runtime path still needs this directive before tightening policy.

---

## RBAC (Role-Based Access Control)

The `service_users` admin endpoints enforce RBAC:

- **Admin scope**: Configurable via `KRAB_AUTH_ADMIN_SCOPE` (default: `admin`)
- **Admin role**: Configurable via `KRAB_AUTH_ADMIN_ROLE` (default: `admin`)
- Requests must include matching scope or role in their JWT claims to access admin endpoints.

---

## Dependency Security

Krab enforces strict dependency governance via `cargo-deny` and the [`deny.toml`](../deny.toml) configuration:

- **Advisories**: All RUSTSEC vulnerabilities are denied. No `ignore` entries are permitted in the configuration.
- **Licenses**: Only allowlisted open-source licenses are accepted (MIT, Apache-2.0, CC0-1.0, BSD-2/3, ISC, Zlib, MPL-2.0, Unicode-3.0, CDLA-Permissive-2.0, Unicode-DFS-2016).
- **Sources**: Unknown registries and git sources are denied — only `crates.io` is allowed.
- **Yanked crates**: Denied.
- **Unmaintained crates**: Flagged.

The CI gate runs:

```sh
cargo deny --all-features check advisories licenses bans
```

Local runs require `cargo-deny` to be installed; without it, the dependency audit gate is unverified.

---

## Threat Mitigations

| Threat               | Mitigation                                                                                                     |
| -------------------- | -------------------------------------------------------------------------------------------------------------- |
| Credential stuffing  | Rate limiting on auth endpoints; burst detection                                                               |
| Token replay         | Single-use refresh tokens with store-backed replay detection                                                   |
| Secret leakage       | `*_FILE` sourcing; inline secrets rejected in production; URL credential redaction in logs                     |
| Supply chain attack  | `cargo-deny` advisories + license + source enforcement                                                         |
| Migration tampering  | Checksum validation on all applied migrations; drift detection                                                 |
| Privilege escalation | RBAC enforcement on admin endpoints; scope/role validation                                                     |
| Timing attacks       | Constant-time comparison (`constant_time_eq`) for token validation; `rsa` crate not present in dependency tree |

---

## Known Limitations

### Instance-Local Rate Limiting

The per-IP rate limiter (`global_rate_limit_middleware`) and auth-failure rate limiter use a distributed store (Redis) for state, but each instance tracks its own window counters. Under horizontal scaling, an attacker can distribute requests across instances to exceed the effective per-IP limit by a factor of the instance count.

**Mitigation before horizontal scaling**: migrate rate limit counters to a shared Redis cluster with atomic `INCR`/`EXPIRE` operations coordinated across all instances.

**Current posture**: acceptable for single-instance deployments. Auth failure counting now fails closed on store unavailability (returns 429) to prevent silent bypass.

### SHA-1 Transitive Dependency

`sha1` v0.10.x is present in the dependency tree as a transitive dependency of `axum` (via `tungstenite`). It is not used directly by any Krab code for security operations. Removal is blocked on upstream `axum`/`tungstenite`. Tracked in `deny.toml` with an explanatory skip annotation.

---

## Reporting Vulnerabilities

Report security vulnerabilities privately via [GitHub Security Advisories](../../security/advisories).

For the dependency governance configuration, see [`deny.toml`](../deny.toml).
