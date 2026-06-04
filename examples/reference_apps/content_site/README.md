# Content Site Reference

Use this track for a small deployable Krab service with static content routes and production-aware defaults.

## Generate

```bash
krab new content-site --template default
cd content-site
cp .env.example .env
krab doctor --diagnostics
cargo run
```

## What To Inspect

- `krab.toml` project model
- `/health` and `/ready`
- generated CI and Kubernetes manifest
- `docs/render_policy.md` for route policy vocabulary

## Extension Points

- Add route handlers for content pages.
- Attach `RouteRenderPolicy` for static or server-rendered paths.
- Run `krab release certify --out release-evidence` before publishing.
