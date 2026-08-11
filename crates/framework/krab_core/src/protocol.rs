use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Versioned RPC envelope for additive schema evolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcEnvelope<T> {
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feature_flags: Vec<String>,
    #[serde(default)]
    pub compatibility_mode: bool,
}

impl<T> RpcEnvelope<T> {
    pub fn new(data: T, version: u32) -> Self {
        Self {
            data,
            request_id: None,
            schema_version: version,
            feature_flags: vec![],
            compatibility_mode: false,
        }
    }

    pub fn with_request_id(mut self, id: String) -> Self {
        self.request_id = Some(id);
        self
    }

    pub fn with_compatibility_mode(mut self) -> Self {
        self.compatibility_mode = true;
        self
    }
}

/// Supported API transport protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolKind {
    Rest,
    Graphql,
    Rpc,
}

impl ProtocolKind {
    /// Parse from a case-insensitive string.
    ///
    /// `"grpc"` is **not** accepted. It used to map to [`Self::Rpc`], so a
    /// service configured with `KRAB_PROTOCOL_ENABLED=grpc` came up exposing
    /// Krab's JSON-over-HTTP RPC and reported itself as satisfying a gRPC
    /// requirement. Krab has no gRPC transport — `tonic` and `prost` appear
    /// nowhere in the workspace — so failing configuration validation is the
    /// honest outcome. See ADR 0007.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rest" => Some(Self::Rest),
            "graphql" => Some(Self::Graphql),
            "rpc" => Some(Self::Rpc),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Graphql => "graphql",
            Self::Rpc => "rpc",
        }
    }
}

/// How many protocol adapters the service exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExposureMode {
    /// Exactly one protocol adapter.
    Single,
    /// Two or more protocol adapters.
    Multi,
}

impl ExposureMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "single" => Some(Self::Single),
            "multi" => Some(Self::Multi),
            _ => None,
        }
    }
}

/// Deployment shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentTopology {
    /// One process serves all adapters.
    SingleService,
    /// Each protocol gets its own microservice binary.
    SplitServices,
}

impl DeploymentTopology {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "single_service" => Some(Self::SingleService),
            "split_services" => Some(Self::SplitServices),
            _ => None,
        }
    }
}

/// Advertised protocol capabilities for a service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceCapabilities {
    pub service: String,
    pub default_protocol: ProtocolKind,
    pub supported_protocols: Vec<ProtocolKind>,
    /// Maps protocol → base route, e.g. Rest → "/api/v1/users".
    pub protocol_routes: HashMap<ProtocolKind, String>,
}

/// Policy constraints on protocol exposure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProtocolPolicy {
    /// Operation name → allowed protocols.
    /// If an operation is listed here, only those protocols are valid.
    pub restricted_operations: HashMap<String, Vec<ProtocolKind>>,
    /// Tenant ID → allowed protocols override.
    ///
    /// If a tenant is listed here, allowed protocols are additionally constrained
    /// to this list.
    pub tenant_overrides: HashMap<String, Vec<ProtocolKind>>,
}

/// Full runtime protocol config loaded from env at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolConfig {
    pub exposure_mode: ExposureMode,
    pub enabled_protocols: Vec<ProtocolKind>,
    pub default_protocol: ProtocolKind,
    pub topology: DeploymentTopology,
    pub policy: ProtocolPolicy,
    /// Disabled by default. When false, `x-krab-protocol` must not be used for switching.
    pub allow_runtime_switch_header: bool,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            exposure_mode: ExposureMode::Single,
            enabled_protocols: vec![ProtocolKind::Rest],
            default_protocol: ProtocolKind::Rest,
            topology: DeploymentTopology::SingleService,
            policy: ProtocolPolicy::default(),
            allow_runtime_switch_header: false,
        }
    }
}

impl ProtocolConfig {
    /// Build from environment variables.
    ///
    /// Env vars consumed:
    /// - `KRAB_PROTOCOL_EXPOSURE_MODE`=single|multi
    /// - `KRAB_PROTOCOL_ENABLED`=rest,graphql,rpc
    /// - `KRAB_PROTOCOL_ENABLED_<SERVICE>`=rest,graphql,rpc (service-local override)
    /// - `KRAB_PROTOCOL_DEFAULT`=rest|graphql|rpc
    /// - `KRAB_PROTOCOL_TOPOLOGY`=single_service|split_services
    /// - `KRAB_PROTOCOL_RESTRICTED_OPS_JSON`={"operation":["rest"]}
    /// - `KRAB_PROTOCOL_TENANT_OVERRIDES_JSON`={"tenant-a":["graphql"]}
    /// - `KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER`=true|false
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_EXPOSURE_MODE") {
            if let Some(mode) = ExposureMode::parse(&raw) {
                config.exposure_mode = mode;
            }
        }

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_DEFAULT") {
            if let Some(protocol) = ProtocolKind::parse(&raw) {
                config.default_protocol = protocol;
            }
        }

        let service_name = std::env::var("KRAB_SERVICE_NAME")
            .or_else(|_| std::env::var("KRAB_SERVICE"))
            .unwrap_or_else(|_| "service".to_string());

        if let Some(service_local) = parse_service_local_enabled(&service_name) {
            config.enabled_protocols = service_local;
        } else if let Ok(raw) = std::env::var("KRAB_PROTOCOL_ENABLED") {
            let parsed = parse_protocol_csv(&raw);
            if !parsed.is_empty() {
                config.enabled_protocols = parsed;
            }
        }

        if !config.enabled_protocols.contains(&config.default_protocol) {
            config.enabled_protocols.push(config.default_protocol);
        }

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_TOPOLOGY") {
            if let Some(topology) = DeploymentTopology::parse(&raw) {
                config.topology = topology;
            }
        }

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_ALLOW_RUNTIME_SWITCH_HEADER") {
            if let Some(value) = parse_bool(&raw) {
                config.allow_runtime_switch_header = value;
            }
        }

        let mut policy = ProtocolPolicy::default();

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_RESTRICTED_OPS_JSON") {
            if let Some(restricted_operations) = parse_protocol_map_json(&raw) {
                policy.restricted_operations = restricted_operations;
            }
        }

        if let Ok(raw) = std::env::var("KRAB_PROTOCOL_TENANT_OVERRIDES_JSON") {
            if let Some(tenant_overrides) = parse_protocol_map_json(&raw) {
                policy.tenant_overrides = tenant_overrides;
            }
        }

        config.policy = policy;

        config
    }

    /// Validate startup invariants.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.enabled_protocols.is_empty() {
            errors.push("enabled_protocols must not be empty".to_string());
        }

        if !self.enabled_protocols.contains(&self.default_protocol) {
            errors.push(format!(
                "default protocol '{}' is not present in enabled_protocols",
                self.default_protocol.as_str()
            ));
        }

        if self.exposure_mode == ExposureMode::Single && self.enabled_protocols.len() != 1 {
            errors.push(format!(
                "single exposure mode requires exactly one enabled protocol (found {})",
                self.enabled_protocols.len()
            ));
        }

        for (operation, protocols) in &self.policy.restricted_operations {
            for protocol in protocols {
                if !self.enabled_protocols.contains(protocol) {
                    errors.push(format!(
                        "restricted operation '{}' contains unsupported protocol '{}'",
                        operation,
                        protocol.as_str()
                    ));
                }
            }
        }

        for (tenant_id, protocols) in &self.policy.tenant_overrides {
            for protocol in protocols {
                if !self.enabled_protocols.contains(protocol) {
                    errors.push(format!(
                        "tenant override '{}' contains unsupported protocol '{}'",
                        tenant_id,
                        protocol.as_str()
                    ));
                }
            }
        }

        if self.topology == DeploymentTopology::SplitServices {
            let split_targets = std::env::var("KRAB_PROTOCOL_SPLIT_TARGETS_JSON")
                .ok()
                .map(|v| v.trim().to_string())
                .unwrap_or_default();
            if split_targets.is_empty() {
                errors.push(
                    "split_services topology requires KRAB_PROTOCOL_SPLIT_TARGETS_JSON".to_string(),
                );
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(feature = "rest")]
pub async fn capabilities_handler(
    axum::extract::State(caps): axum::extract::State<ServiceCapabilities>,
) -> axum::Json<ServiceCapabilities> {
    axum::Json(caps)
}

fn parse_bool(value: &str) -> Option<bool> {
    let normalized = value.trim();
    if normalized.eq_ignore_ascii_case("true") || normalized == "1" {
        return Some(true);
    }
    if normalized.eq_ignore_ascii_case("false") || normalized == "0" {
        return Some(false);
    }
    None
}

fn parse_protocol_csv(value: &str) -> Vec<ProtocolKind> {
    let mut out = Vec::new();
    for token in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(protocol) = ProtocolKind::parse(token) {
            if !out.contains(&protocol) {
                out.push(protocol);
            }
        }
    }
    out
}

fn parse_protocol_map_json(value: &str) -> Option<HashMap<String, Vec<ProtocolKind>>> {
    let raw: HashMap<String, Vec<String>> = serde_json::from_str(value).ok()?;
    let mut parsed = HashMap::new();
    for (key, protocols) in raw {
        let mut out = Vec::new();
        for protocol in protocols {
            if let Some(kind) = ProtocolKind::parse(&protocol) {
                if !out.contains(&kind) {
                    out.push(kind);
                }
            }
        }
        parsed.insert(key, out);
    }
    Some(parsed)
}

fn parse_service_local_enabled(service_name: &str) -> Option<Vec<ProtocolKind>> {
    let suffix = service_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();

    let key = format!("KRAB_PROTOCOL_ENABLED_{suffix}");
    let raw = std::env::var(key).ok()?;
    let parsed = parse_protocol_csv(&raw);
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}
