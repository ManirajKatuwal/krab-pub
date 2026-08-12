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
- [`docs/architecture/render_policy.md`](../../../docs/architecture/render_policy.md)
  for route policy vocabulary. This track scaffolds with `--template default`,
  which emits no `docs/` directory of its own; the `edge-ssr` template is the
  one that generates a project-local `docs/render_policy.md`.

## Extension Points

- Add route handlers for content pages.
- Attach `RouteRenderPolicy` for static or server-rendered paths.
- Run `cargo test` and the generated `.github/workflows/ci.yaml` before
  publishing. `krab release certify` is **not** the gate here: it is a
  governance command hardcoded to the framework workspace's own services, so it
  does not apply to a generated project. `krab doctor --strict` does apply, and
  is the closest per-project equivalent.
