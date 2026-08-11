use std::collections::HashMap;

use krab_core::protocol::{ProtocolConfig, ProtocolKind, ServiceCapabilities};

pub fn build_capabilities(config: &ProtocolConfig) -> ServiceCapabilities {
    let mut routes = HashMap::new();

    if config.enabled_protocols.contains(&ProtocolKind::Rest) {
        routes.insert(ProtocolKind::Rest, "/api/v1/users".to_string());
    }
    if config.enabled_protocols.contains(&ProtocolKind::Graphql) {
        routes.insert(ProtocolKind::Graphql, "/api/v1/graphql".to_string());
    }
    if config.enabled_protocols.contains(&ProtocolKind::Rpc) {
        routes.insert(ProtocolKind::Rpc, "/api/v1/rpc".to_string());
    }

    ServiceCapabilities {
        service: "users".to_string(),
        default_protocol: config.default_protocol,
        supported_protocols: config.enabled_protocols.clone(),
        protocol_routes: routes,
    }
}
