# Event-Stream Reference

Use this track for dashboards, live status pages, and long-lived browser connections.

## Generate

```bash
krab new event-stream --template event-stream
cd event-stream
cp .env.example .env
krab doctor --diagnostics
cargo run
```

## What To Inspect

- `/api/events` SSE endpoint
- `/api/ws` WebSocket endpoint
- `/api/dashboard` JSON state endpoint
- readiness and liveness routes

## Extension Points

- Add authentication to stream endpoints.
- Add backpressure and disconnect accounting.
- Export stream metrics through Prometheus.
