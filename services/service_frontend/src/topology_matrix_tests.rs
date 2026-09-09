//! Ambient-environment coverage for the `topology-matrix` CI gate.
//!
//! `.github/workflows/topology-matrix.yaml` runs one job per topology leg and
//! exports `KRAB_RUNTIME_TOPOLOGY` / `KRAB_RUNTIME_ENDPOINTS_JSON` as job env,
//! then runs `cargo test -p service_frontend`. Until this module existed those
//! two variables were read by nothing the test binary touched — every
//! topology-sensitive test built a `TopologyRuntime` literal, so both legs
//! executed byte-identical code and the matrix proved nothing.
//!
//! The tests here read the *ambient* environment the way `main` does, via
//! `TopologyRuntime::from_env_checked`, and then assert the consequences that
//! mode actually has on frontend wiring:
//!
//! - `resolve_service_base_url` prefers a distributed endpoint over the
//!   `KRAB_*_BASE_URL` fallback, and must not do so in monolith mode;
//! - `build_users_adapter` selects the remote REST adapter in distributed mode
//!   and the in-process one otherwise.
//!
//! Because they branch on the ambient value rather than setting it, the two
//! matrix legs take different assertion paths, and a plain
//! `cargo test -p service_frontend` with no topology env set still passes
//! against the documented default (monolith, empty endpoint map).
//!
//! The module is serialized with the same `#[serial_test::serial]` key as the
//! other env-reading suites in this crate (`main.rs`, `cache.rs`), so the
//! `KRAB_*_BASE_URL` fallbacks those tests mutate cannot race these.

use std::collections::HashMap;

use krab_core::service_contract::{ServiceEndpoint, ServiceTopology, TopologyRuntime};

use crate::frontend_env::resolve_service_base_url;
use crate::users_contract::{build_users_adapter, UsersAdapterKind};

/// Fallback value planted in `KRAB_*_BASE_URL` so a distributed leg that
/// silently fell back to the env path is distinguishable from one that used its
/// endpoint map. No leg of the matrix should ever resolve to this in
/// distributed mode.
const FALLBACK_DECOY: &str = "http://127.0.0.1:59999";

/// The ambient topology leg this test process is running under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmbientLeg {
    /// `KRAB_RUNTIME_TOPOLOGY` is absent — a local run or a non-matrix workflow.
    Unset,
    Monolith,
    Distributed,
}

fn ambient_leg() -> AmbientLeg {
    let Ok(raw) = std::env::var("KRAB_RUNTIME_TOPOLOGY") else {
        return AmbientLeg::Unset;
    };

    match ServiceTopology::parse(&raw) {
        Some(ServiceTopology::Monolith) => AmbientLeg::Monolith,
        Some(ServiceTopology::Distributed) => AmbientLeg::Distributed,
        None => panic!(
            "ambient KRAB_RUNTIME_TOPOLOGY='{}' is not a value the runtime accepts; \
             service_frontend startup would reject it too (expected one of \
             monolith|single|single_service|distributed|split|split_services)",
            raw.trim()
        ),
    }
}

/// Resolve the ambient topology exactly as `main` does, so a matrix leg whose
/// env is internally inconsistent (distributed with no endpoints, unparseable
/// endpoint JSON) fails the gate instead of degrading to monolith.
fn ambient_topology() -> TopologyRuntime {
    TopologyRuntime::from_env_checked().unwrap_or_else(|error| {
        panic!(
            "ambient topology env must be startable: {error}\n(service_frontend's main() calls \
             TopologyRuntime::from_env_checked() and would abort on this)"
        )
    })
}

/// Sets an env var for the duration of a test and restores whatever was there
/// before — including "nothing" — on drop.
struct EnvGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
#[serial_test::serial]
fn ambient_topology_env_drives_users_base_url_and_adapter_selection() {
    let leg = ambient_leg();
    let topology = ambient_topology();

    let _decoy = EnvGuard::set("KRAB_USERS_BASE_URL", FALLBACK_DECOY);
    let resolved = resolve_service_base_url(
        &topology,
        "users",
        "KRAB_USERS_BASE_URL",
        "http://127.0.0.1:3002",
    );
    let bundle = build_users_adapter(&topology, resolved.clone(), None);

    match leg {
        AmbientLeg::Unset => {
            assert_eq!(
                topology.mode,
                ServiceTopology::Monolith,
                "absent KRAB_RUNTIME_TOPOLOGY must mean the documented monolith default"
            );
            // Only when the endpoint map is absent too. A developer with
            // KRAB_RUNTIME_ENDPOINTS_JSON exported but no KRAB_RUNTIME_TOPOLOGY
            // is still in the documented default, and the assertion that
            // matters for them is the one below: monolith mode ignores the map.
            if std::env::var_os("KRAB_RUNTIME_ENDPOINTS_JSON").is_none() {
                assert!(
                    topology.endpoints.is_empty(),
                    "absent topology env must leave the endpoint map empty, got {:?}",
                    topology.endpoints
                );
            }
            assert_eq!(
                resolved, FALLBACK_DECOY,
                "with no topology env the KRAB_USERS_BASE_URL fallback must be authoritative"
            );
            assert_eq!(
                bundle.kind,
                UsersAdapterKind::LocalInProcess,
                "the default topology must wire the in-process users adapter"
            );
        }
        AmbientLeg::Monolith => {
            assert_eq!(
                topology.mode,
                ServiceTopology::Monolith,
                "the monolith matrix leg must resolve to ServiceTopology::Monolith"
            );
            // The monolith leg is handed an endpoint map on purpose: proving it
            // is *ignored* is the whole point of that leg.
            assert_eq!(
                resolved, FALLBACK_DECOY,
                "monolith mode must ignore KRAB_RUNTIME_ENDPOINTS_JSON ({:?}) and use the \
                 KRAB_USERS_BASE_URL fallback",
                topology.endpoints
            );
            assert_eq!(
                bundle.kind,
                UsersAdapterKind::LocalInProcess,
                "monolith mode must wire the in-process users adapter even with endpoints present"
            );
        }
        AmbientLeg::Distributed => {
            assert_eq!(
                topology.mode,
                ServiceTopology::Distributed,
                "the distributed matrix leg must resolve to ServiceTopology::Distributed"
            );
            let endpoint = topology.endpoint_for("users").unwrap_or_else(|| {
                panic!(
                    "the distributed leg must publish a 'users' endpoint in \
                     KRAB_RUNTIME_ENDPOINTS_JSON; got keys {:?}",
                    topology.endpoints.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                resolved,
                endpoint.base_url.trim().trim_end_matches('/'),
                "distributed mode must resolve the users base URL from the endpoint map"
            );
            assert_ne!(
                resolved, FALLBACK_DECOY,
                "distributed mode must not fall back to KRAB_USERS_BASE_URL"
            );
            assert_eq!(
                bundle.kind,
                UsersAdapterKind::RemoteRest,
                "distributed mode must wire the remote REST users adapter"
            );
        }
    }
}

#[test]
#[serial_test::serial]
fn ambient_topology_env_drives_auth_base_url_resolution() {
    let leg = ambient_leg();
    let topology = ambient_topology();

    let _decoy = EnvGuard::set("KRAB_AUTH_BASE_URL", FALLBACK_DECOY);
    let resolved = resolve_service_base_url(
        &topology,
        "auth",
        "KRAB_AUTH_BASE_URL",
        "http://127.0.0.1:3001",
    );

    match leg {
        AmbientLeg::Unset | AmbientLeg::Monolith => {
            assert_eq!(
                resolved, FALLBACK_DECOY,
                "non-distributed topology must resolve auth from KRAB_AUTH_BASE_URL"
            );
        }
        AmbientLeg::Distributed => {
            let endpoint = topology.endpoint_for("auth").unwrap_or_else(|| {
                panic!(
                    "the distributed leg must publish an 'auth' endpoint in \
                     KRAB_RUNTIME_ENDPOINTS_JSON; got keys {:?}",
                    topology.endpoints.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                resolved,
                endpoint.base_url.trim().trim_end_matches('/'),
                "distributed mode must resolve the auth base URL from the endpoint map"
            );
            assert_ne!(
                resolved, FALLBACK_DECOY,
                "distributed mode must not fall back to KRAB_AUTH_BASE_URL"
            );
        }
    }
}

/// The ambient tests above are only as good as the divergence they can see, so
/// pin the mechanism they rely on against explicit literals too: one endpoint
/// map, two modes, two different results.
#[test]
#[serial_test::serial]
fn the_two_modes_diverge_on_identical_endpoint_maps() {
    let endpoints = HashMap::from([(
        "users".to_string(),
        ServiceEndpoint {
            base_url: "http://127.0.0.1:3002".to_string(),
            timeout_ms: 1200,
            max_retries: 1,
        },
    )]);

    let _decoy = EnvGuard::set("KRAB_USERS_BASE_URL", FALLBACK_DECOY);

    let monolith = TopologyRuntime {
        mode: ServiceTopology::Monolith,
        endpoints: endpoints.clone(),
    };
    let distributed = TopologyRuntime {
        mode: ServiceTopology::Distributed,
        endpoints,
    };

    let monolith_url = resolve_service_base_url(
        &monolith,
        "users",
        "KRAB_USERS_BASE_URL",
        "http://127.0.0.1:3002",
    );
    let distributed_url = resolve_service_base_url(
        &distributed,
        "users",
        "KRAB_USERS_BASE_URL",
        "http://127.0.0.1:3002",
    );

    assert_eq!(monolith_url, FALLBACK_DECOY);
    assert_eq!(distributed_url, "http://127.0.0.1:3002");
    assert_ne!(
        monolith_url, distributed_url,
        "the two topology modes must not resolve to the same base URL"
    );

    assert_eq!(
        build_users_adapter(&monolith, monolith_url, None).kind,
        UsersAdapterKind::LocalInProcess
    );
    assert_eq!(
        build_users_adapter(&distributed, distributed_url, None).kind,
        UsersAdapterKind::RemoteRest
    );
}
