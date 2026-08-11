# SaaS Dashboard Reference

Use this track for auth-ready service scaffolding, tenant APIs, release checks, and operational defaults.

## Generate

```bash
krab new saas-dashboard --template saas
cd saas-dashboard
cp .env.example .env
krab doctor --diagnostics
cargo run
```

## What To Inspect

- `apply_common_http_layers`
- `/api/v1/tenants`
- generated `KRAB_AUTH_MODE`, OIDC, and database environment placeholders
- generated readiness and startup probes

## Extension Points

- Add an auth implementation or connect to `service_auth`.
- Add tenant persistence behind repository traits.
- Use `ServerFnError::validation`, `ServerFnError::unauthorized`, and `ServerFnError::forbidden` for public mutation endpoints.
