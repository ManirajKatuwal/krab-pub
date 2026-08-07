# Environment Template and Validation

Use [`.env.example`](../../.env.example) as the baseline for local/dev setup.

## Required variables

- `KRAB_ENVIRONMENT`: `local|dev|staging|prod`
- `KRAB_AUTH_MODE`: `static|jwt`
- `DB_MIGRATION_ALLOW_APPLY`: `true|false`
- `DB_MIGRATION_FAILURE_POLICY`: `halt|continue_non_critical`

## SEO / public URL

- `KRAB_PUBLIC_BASE_URL`: public origin used by frontend SEO metadata (`canonical`, `og:url`) and crawler endpoints (`/robots.txt`, `/sitemap.xml`).
- Local default is `http://localhost:3000`, but set this explicitly in staging/prod to your external HTTPS URL.

## Conditional variables

When `KRAB_AUTH_MODE=jwt`, the following are required:

- `KRAB_OIDC_ISSUER`
- `KRAB_OIDC_AUDIENCE`

In `staging` / `prod`, secret material must be sourced from vault or `*_FILE` variables before release:

- JWT signing secrets: one of `KRAB_JWT_SECRET_FILE`, `KRAB_JWT_KEYS_JSON_FILE`, `KRAB_JWT_SECRET_VAULT_REF`, `KRAB_JWT_KEYS_JSON_VAULT_REF`
- Auth login/bootstrap secrets: one of `KRAB_AUTH_BOOTSTRAP_PASSWORD_FILE`, `KRAB_AUTH_LOGIN_USERS_JSON_FILE`, `KRAB_AUTH_BOOTSTRAP_PASSWORD_VAULT_REF`, `KRAB_AUTH_LOGIN_USERS_JSON_VAULT_REF`

Inline `KRAB_JWT_SECRET` / `KRAB_JWT_KEYS_JSON` is rejected in non-local environments unless secure `*_FILE` / `*_VAULT_REF` sourcing is also configured.

## Validation command

Run:

```bash
cargo run -p krab_cli -- env-check --strict
```

This validates required/conditional settings and fails in strict mode on warnings.

## One-command local stack bootstrap

Run:

```bash
cargo run -p krab_cli -- bootstrap
```

This performs build + orchestrator startup for deterministic onboarding.
