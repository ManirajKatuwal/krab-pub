# Migration Guide

This guide maps common framework concepts to Krab equivalents. It is intentionally conceptual; use the reference apps for concrete layouts.

---

## Upgrading within Krab

### Unreleased — `IsrCache` becomes async and store-backed

**Breaking, API.** `IsrCache` kept pages in a process-local `HashMap`, which is
silently wrong the moment you run more than one replica: each holds its own copy
and invalidation reaches only one. It now sits on
`krab_core::store::DistributedStore`.

**What changes at the call site:** every method is `async` and returns
`anyhow::Result`.

```rust
// Before
if let Some(entry) = cache.get("/blog/hello") { … }
cache.put("/blog/hello", html, policy);
let removed = cache.invalidate_prefix("/blog");

// After
if let Some(entry) = cache.get("/blog/hello").await? { … }
cache.put("/blog/hello", html, policy).await?;
let removed = cache.invalidate_prefix("/blog").await?;
```

`IsrEntry::generated_at` is now a `SystemTime` rather than an `Instant`. If you
read it directly, `entry.age()` is unchanged and is the better call.

**To actually get shared caching**, build it over your store instead of
`IsrCache::new()`:

```rust,ignore
let runtime = RuntimeState::try_new()?;             // reads KRAB_REDIS_URL, fails closed outside dev
let isr_cache = IsrCache::with_store(runtime.store.clone());
```

`IsrCache::new()` still works and still means one process only — that is now
documented rather than implied. If you deploy a single instance, no change is
needed beyond the `.await`s.

**If you implement `DistributedStore` yourself**, add `delete`,
`keys_with_prefix`, and (new in 0.3.0) `set_if_absent`. Prefix scans must not
block: use `SCAN`, not `KEYS`.

### 0.3.0 — cleaning up orphaned ISR cache keys (Redis-backed ISR only)

The ISR key separator changed from `:` to the control byte ``, so entries
written under the old format are never read again and repopulate automatically
under the new format. TTL'd entries (`Revalidate`, `OnDemand`) age out on their
own. **`Static` entries have no TTL and will linger in Redis unread** until
deleted. This is a memory leak only — never a correctness issue. Skip this
entirely if you do not use `KRAB_REDIS_URL` with ISR.

Old keys have the shape `<namespace>:<path>` and paths always begin with `/`
(default namespace is `krab:isr`). **Preview first:**

```sh
redis-cli --scan --pattern 'krab:isr:/*'
```

When the list looks right, delete in a non-blocking pass:

```sh
redis-cli --scan --pattern 'krab:isr:/*' | xargs -r -L 100 redis-cli del
```

Safety caveats:

- Use `--scan` (SCAN), never `KEYS` — `KEYS` blocks the single Redis thread
  across the whole keyspace.
- The `/` after `krab:isr:` is load-bearing: it matches old colon-separated
  entries (`krab:isr:/blog/x`) while **excluding** new-format sub-namespace keys
  such as `krab:isr:site/blog/x`. Do **not** broaden to `krab:isr:*` —
  that glob would also match live new-format keys of any namespace containing a
  colon.
- For a custom namespace (e.g. `krab:isr:site`), use
  `--pattern 'krab:isr:site:/*'`.
- Deleting old keys is safe at any time, including while serving: the running
  framework only reads ``-separated keys.

### Unreleased — login credentials become Argon2id hashes

**Breaking, operator-facing.** `KRAB_AUTH_LOGIN_USERS_JSON` and
`KRAB_AUTH_BOOTSTRAP_PASSWORD` held plaintext passwords, compared with `!=`.
They now hold Argon2id hashes in PHC string format, verified with a
constant-time Argon2 verification.

**Who is affected:** any deployment of `service_auth` in an environment other
than `dev`/`local`. Those two still accept a plaintext value and hash it at
startup, so local development needs no change.

**What breaks if you do nothing:** startup fails with a message naming the
offending variable and user. It fails closed — a plaintext credential is never
silently accepted in a non-local environment.

**Migration:**

1. Hash each password:

   ```sh
   krab auth hash-password --username admin
   # reads the password from stdin so it stays out of shell history
   ```

2. Replace the values in your secret store. `KRAB_AUTH_LOGIN_USERS_JSON` keeps
   the same shape — a JSON object keyed by username — with hashes as values:

   ```json
   { "admin": "$argon2id$v=19$m=19456,t=2,p=1$<salt>$<digest>" }
   ```

3. Redeploy. The `*_FILE` and `*_VAULT_REF` sourcing rules are unchanged; only
   the value format differs.

There is no compatibility window on this one. A plaintext password accepted for
one more minor version is a plaintext password in production for one more minor
version, so the deprecation convention in
[`RELEASE_POLICY.md`](../../RELEASE_POLICY.md) is deliberately not applied here.
See [`docs/reference/security.md`](../reference/security.md#password-credentials).

---

## From Axum

| Axum concept | Krab equivalent |
| --- | --- |
| `Router` and handlers | Keep Axum handlers, then add Krab HTTP layers, service config, and release checks |
| Manual health routes | Standard `/health` and `/ready` routes in generated starters |
| Local process scripts | `krab bootstrap` with supervised startup order and readiness checks |
| Ad hoc CI | `krab doctor` and `krab release certify` evidence bundles |

Recommended path:

1. Move existing routes behind a Krab project model in `krab.toml`.
2. Add `/health` and `/ready`.
3. Apply common HTTP layers and runtime state.
4. Add release certification to CI before changing behavior.

## From Leptos

| Leptos concept | Krab equivalent |
| --- | --- |
| Server functions | `#[server]` functions mounted under `/api/rpc/{name}` |
| Islands | Krab islands with explicit hydration markers |
| SSR policy | `RouteRenderPolicy` with `RenderMode` and `CacheMode` |
| Full-stack app shell | Krab service plus orchestrator and release tooling |

Key difference: Krab treats server functions as public HTTP endpoints and documents validation/auth responsibilities explicitly.

## From Next.js, Astro, SvelteKit, or Nuxt

| Concept | Krab equivalent |
| --- | --- |
| API routes / server actions | `#[server]` functions or Axum handlers |
| Route rendering modes | `RouteRenderPolicy` |
| Islands / partial hydration | Krab island components with server-emitted hydration markers |
| Platform adapters | Rust service deployment plus orchestrator config |
| Preview/deploy checks | `krab doctor` and `krab release certify` |

Krab is not trying to mirror every frontend convention. The trade is Rust-native service composition, explicit operations defaults, and one project model for local development through release evidence.
