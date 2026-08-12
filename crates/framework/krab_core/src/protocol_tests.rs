use std::collections::HashMap;

use axum::body::Body;
use axum::http::Request;

use crate::http::{route_family_protocol, runtime_switch_header_rejected_by_default};
use crate::protocol::{
    DeploymentTopology, ExposureMode, ProtocolConfig, ProtocolKind, ProtocolPolicy,
    ServiceCapabilities,
};

#[test]
fn test_topology_parse_single_service() {
    assert_eq!(
        DeploymentTopology::parse("single_service"),
        Some(DeploymentTopology::SingleService)
    );
}

#[test]
fn test_topology_parse_split_services() {
    assert_eq!(
        DeploymentTopology::parse("split_services"),
        Some(DeploymentTopology::SplitServices)
    );
}

#[test]
fn test_config_validation_default_in_enabled() {
    let config = ProtocolConfig {
        exposure_mode: ExposureMode::Multi,
        enabled_protocols: vec![ProtocolKind::Rest],
        default_protocol: ProtocolKind::Graphql,
        topology: DeploymentTopology::SingleService,
        policy: ProtocolPolicy::default(),
        allow_runtime_switch_header: false,
    };

    let errors = config.validate().expect_err("validation should fail");
    assert!(errors.iter().any(|e| e.contains("default protocol")));
}

#[test]
fn test_config_validation_single_mode_one_protocol() {
    let config = ProtocolConfig {
        exposure_mode: ExposureMode::Single,
        enabled_protocols: vec![ProtocolKind::Rest, ProtocolKind::Graphql],
        default_protocol: ProtocolKind::Rest,
        topology: DeploymentTopology::SingleService,
        policy: ProtocolPolicy::default(),
        allow_runtime_switch_header: false,
    };

    let errors = config.validate().expect_err("validation should fail");
    assert!(errors.iter().any(|e| e.contains("single exposure mode")));
}

fn valid_rest_config() -> ProtocolConfig {
    ProtocolConfig {
        exposure_mode: ExposureMode::Multi,
        enabled_protocols: vec![ProtocolKind::Rest],
        default_protocol: ProtocolKind::Rest,
        topology: DeploymentTopology::SingleService,
        policy: ProtocolPolicy::default(),
        allow_runtime_switch_header: false,
    }
}

/// Malformed policy JSON used to parse to `None` and silently drop every
/// restriction (fail-open). `validate()` must surface it as a startup error.
#[test]
#[serial_test::serial]
fn test_config_validation_rejects_malformed_policy_json() {
    std::env::set_var("KRAB_PROTOCOL_RESTRICTED_OPS_JSON", "{not json");
    let result = valid_rest_config().validate();
    std::env::remove_var("KRAB_PROTOCOL_RESTRICTED_OPS_JSON");

    let errors = result.expect_err("validation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("KRAB_PROTOCOL_RESTRICTED_OPS_JSON") && e.contains("not valid JSON")));
}

/// An unknown protocol name inside valid JSON used to become an empty list
/// (silent deny-all) with no diagnostic.
#[test]
#[serial_test::serial]
fn test_config_validation_rejects_unknown_protocol_name_in_policy_json() {
    std::env::set_var(
        "KRAB_PROTOCOL_TENANT_OVERRIDES_JSON",
        r#"{"tenant-a":["grpc"]}"#,
    );
    let result = valid_rest_config().validate();
    std::env::remove_var("KRAB_PROTOCOL_TENANT_OVERRIDES_JSON");

    let errors = result.expect_err("validation should fail");
    assert!(errors.iter().any(|e| e.contains("unknown protocol 'grpc'")));
}

#[test]
fn test_config_validation_tenant_override_protocol_must_be_enabled() {
    let mut tenant_overrides = HashMap::new();
    tenant_overrides.insert("tenant-a".to_string(), vec![ProtocolKind::Rpc]);

    let config = ProtocolConfig {
        exposure_mode: ExposureMode::Single,
        enabled_protocols: vec![ProtocolKind::Rest],
        default_protocol: ProtocolKind::Rest,
        topology: DeploymentTopology::SingleService,
        policy: ProtocolPolicy {
            restricted_operations: HashMap::new(),
            tenant_overrides,
        },
        allow_runtime_switch_header: false,
    };

    let errors = config.validate().expect_err("validation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("tenant override") && e.contains("unsupported protocol")));
}

#[test]
fn test_parse_protocol_kind_case_insensitive() {
    assert_eq!(ProtocolKind::parse("REST"), Some(ProtocolKind::Rest));
    assert_eq!(ProtocolKind::parse("Graphql"), Some(ProtocolKind::Graphql));
    assert_eq!(ProtocolKind::parse("RPC"), Some(ProtocolKind::Rpc));
    // `GRPC` asserted `Some(Rpc)` here until ADR 0007. See
    // `protocol_parse_rejects_grpc_instead_of_aliasing_it_to_rpc`.
}

#[test]
fn test_parse_protocol_kind_invalid() {
    assert_eq!(ProtocolKind::parse("soap"), None);
    assert_eq!(ProtocolKind::parse(""), None);
    assert_eq!(ProtocolKind::parse("xml"), None);
}

#[test]
fn test_route_family_resolves_protocol_rest() {
    assert_eq!(
        route_family_protocol("/api/v1/users/me"),
        Some(ProtocolKind::Rest)
    );
}

#[test]
fn test_route_family_resolves_protocol_graphql() {
    assert_eq!(
        route_family_protocol("/api/v1/graphql"),
        Some(ProtocolKind::Graphql)
    );
}

#[test]
fn test_route_family_resolves_protocol_rpc() {
    assert_eq!(
        route_family_protocol("/api/v1/rpc"),
        Some(ProtocolKind::Rpc)
    );
}

#[test]
fn test_runtime_switch_header_rejected_by_default() {
    let req = Request::builder()
        .uri("/api/v1/users/me")
        .header("x-krab-protocol", "graphql")
        .body(Body::empty())
        .expect("request should build");

    let config = ProtocolConfig {
        exposure_mode: ExposureMode::Single,
        enabled_protocols: vec![ProtocolKind::Rest],
        default_protocol: ProtocolKind::Rest,
        topology: DeploymentTopology::SingleService,
        policy: ProtocolPolicy::default(),
        allow_runtime_switch_header: false,
    };

    assert!(runtime_switch_header_rejected_by_default(&req, &config));
}

#[test]
fn test_capabilities_struct_shape_is_constructible() {
    let mut routes = HashMap::new();
    routes.insert(ProtocolKind::Rest, "/api/v1/users".to_string());
    let caps = ServiceCapabilities {
        service: "users".to_string(),
        default_protocol: ProtocolKind::Rest,
        supported_protocols: vec![ProtocolKind::Rest],
        protocol_routes: routes,
    };

    assert_eq!(caps.service, "users");
    assert_eq!(caps.default_protocol, ProtocolKind::Rest);
    assert_eq!(caps.supported_protocols, vec![ProtocolKind::Rest]);
    assert_eq!(
        caps.protocol_routes.get(&ProtocolKind::Rest),
        Some(&"/api/v1/users".to_string())
    );
}

/// ADR 0007 — `grpc` is not a Krab transport, and configuration must say so
/// rather than silently substituting Krab's JSON-over-HTTP RPC.
#[test]
fn protocol_parse_rejects_grpc_instead_of_aliasing_it_to_rpc() {
    assert_eq!(ProtocolKind::parse("rpc"), Some(ProtocolKind::Rpc));

    // Previously `Some(ProtocolKind::Rpc)`. A service configured with
    // `KRAB_PROTOCOL_ENABLED=grpc` came up exposing Krab RPC and reported
    // itself as satisfying a gRPC requirement it cannot satisfy.
    assert_eq!(ProtocolKind::parse("grpc"), None);
    assert_eq!(ProtocolKind::parse("GRPC"), None);
}

/// The supported set is exactly three, and none of them is gRPC.
#[test]
fn protocol_supported_set_is_rest_graphql_rpc() {
    for (input, expected) in [
        ("rest", ProtocolKind::Rest),
        ("graphql", ProtocolKind::Graphql),
        ("rpc", ProtocolKind::Rpc),
    ] {
        assert_eq!(ProtocolKind::parse(input), Some(expected));
    }

    for unsupported in ["grpc", "tonic", "soap", ""] {
        assert_eq!(ProtocolKind::parse(unsupported), None, "{unsupported}");
    }
}
