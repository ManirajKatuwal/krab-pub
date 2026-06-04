# BOOTSTRAP-SLICE-01 Inventory — Duplicate Startup Patterns

Last updated: 2026-03-27

This inventory captures repeated startup/runtime assembly patterns across:

- [`services/service_auth/src/main.rs`](services/service_auth/src/main.rs)
- [`services/service_users/src/lib.rs`](services/service_users/src/lib.rs)
- [`services/service_frontend/src/main.rs`](services/service_frontend/src/main.rs)

## Pattern matrix

| Pattern                               | Auth                                                                                                                                             | Users                                                                                                                                                       | Frontend                                                                                                                                                       | Extraction candidate                                                                                                        |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| Tracing bootstrap                     | [`init_tracing("service_auth")`](services/service_auth/src/main.rs:708)                                                                          | [`init_tracing("service_users")`](services/service_users/src/lib.rs:563) and [`init_tracing(target.service_name())`](services/service_users/src/lib.rs:570) | [`init_tracing("service_frontend")`](services/service_frontend/src/main.rs:1291)                                                                               | Add shared startup entry helper in [`crates/framework/krab_core/src/service.rs`](crates/framework/krab_core/src/service.rs) |
| Typed config load + validate          | [`KrabConfig::from_env_checked("auth", 3001)`](services/service_auth/src/main.rs:622), [`cfg.validate()`](services/service_auth/src/main.rs:624) | [`KrabConfig::from_env_checked("users", 3002)`](services/service_users/src/lib.rs:336), [`cfg.validate()`](services/service_users/src/lib.rs:338)           | [`KrabConfig::from_env_checked("frontend", 3000)`](services/service_frontend/src/main.rs:1292), [`cfg.validate()`](services/service_frontend/src/main.rs:1293) | Shared `load_validated_config(service, default_port)` helper                                                                |
| Runtime-state construction            | [`RuntimeState::new()`](services/service_auth/src/main.rs:574)                                                                                   | [`RuntimeState::new()`](services/service_users/src/lib.rs:299)                                                                                              | [`RuntimeState::new()`](services/service_frontend/src/main.rs:1346)                                                                                            | Shared state bootstrap helper for common runtime fields                                                                     |
| Router + common HTTP layers           | [`build_app()`](services/service_auth/src/main.rs:549) + [`apply_common_http_layers()`](services/service_auth/src/main.rs:567)                   | [`build_app()`](services/service_users/src/lib.rs:252) + [`apply_common_http_layers()`](services/service_users/src/lib.rs:282)                              | [`build_router()`](services/service_frontend/src/main.rs:1219) + [`apply_common_http_layers()`](services/service_frontend/src/main.rs:1286)                    | Common router finalization helper (apply layers + state attach)                                                             |
| Bind and serve with graceful shutdown | [`TcpListener::bind()`](services/service_auth/src/main.rs:588) + [`axum::serve()`](services/service_auth/src/main.rs:591)                        | [`TcpListener::bind()`](services/service_users/src/lib.rs:318) + [`axum::serve()`](services/service_users/src/lib.rs:321)                                   | [`TcpListener::bind()`](services/service_frontend/src/main.rs:1367) + [`axum::serve()`](services/service_frontend/src/main.rs:1369)                            | Shared serve helper (bind, log, graceful shutdown)                                                                          |

## Notes

1. Highest structural duplication is between auth and users service startup flows.
2. Frontend shares the same core sequence but carries additional topology and HMR concerns (expected divergence).
3. Inventory confirms BOOTSTRAP-SLICE-02 should focus on helpers that preserve customization hooks while centralizing repeated startup mechanics.

## Recommended BOOTSTRAP-SLICE-02 helper candidates

- `init_service_tracing(service_name: &str)`
- `load_validated_service_config(service: &str, default_port: u16)`
- `serve_with_graceful_shutdown(app, addr, service_name)`
