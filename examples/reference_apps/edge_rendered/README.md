# Edge-Rendered Reference

Use this track when route rendering behavior is the main design axis.

## Generate

```bash
krab new edge-rendered --template edge-ssr
cd edge-rendered
cp .env.example .env
krab doctor --diagnostics
cargo run
```

## What To Inspect

- `RouteRenderPolicy`
- `RenderMode::Server`
- `CacheMode::Isr`
- `EdgeCapability::Eligible`
- `with_streaming(true)`

## Extension Points

- Replace the skeleton HTML handler with real SSR.
- Wire cache reads and revalidation through the policy object.
- Use `docs/render_policy.md` to choose SSR, ISR, SWR, static, or uncached behavior.
