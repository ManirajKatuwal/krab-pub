use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Runtime topology mode used by contract adapter wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTopology {
    Monolith,
    Distributed,
}

impl ServiceTopology {
    /// Parse from env/config string values.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "monolith" | "single" | "single_service" => Some(Self::Monolith),
            "distributed" | "split" | "split_services" => Some(Self::Distributed),
            _ => None,
        }
    }
}

/// Endpoint configuration used by remote adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    pub base_url: String,
    pub timeout_ms: u64,
    pub max_retries: u8,
}

impl Default for ServiceEndpoint {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:3000".to_string(),
            timeout_ms: 1_500,
            max_retries: 2,
        }
    }
}

/// Topology runtime config used by service adapters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyRuntime {
    pub mode: ServiceTopology,
    /// Domain name => endpoint config
    pub endpoints: HashMap<String, ServiceEndpoint>,
}

impl Default for TopologyRuntime {
    fn default() -> Self {
        Self {
            mode: ServiceTopology::Monolith,
            endpoints: HashMap::new(),
        }
    }
}

impl TopologyRuntime {
    /// Build topology runtime from environment.
    ///
    /// Consumed env vars:
    /// - KRAB_RUNTIME_TOPOLOGY=monolith|distributed
    /// - KRAB_RUNTIME_ENDPOINTS_JSON={"users":{"base_url":"http://127.0.0.1:3002","timeout_ms":1500,"max_retries":2}}
    pub fn from_env() -> Self {
        let mut out = Self::default();

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_TOPOLOGY") {
            if let Some(mode) = ServiceTopology::parse(&raw) {
                out.mode = mode;
            }
        }

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_ENDPOINTS_JSON") {
            if let Ok(endpoints) = serde_json::from_str::<HashMap<String, ServiceEndpoint>>(&raw) {
                out.endpoints = endpoints;
            }
        }

        out
    }

    pub fn endpoint_for(&self, domain: &str) -> Option<&ServiceEndpoint> {
        self.endpoints.get(domain)
    }
}

/// Stable domain error categories that can be mapped to transport status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainErrorKind {
    Validation,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Timeout,
    UpstreamUnavailable,
    Internal,
}

/// Transport-agnostic domain error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainError {
    pub kind: DomainErrorKind,
    pub code: String,
    pub message: String,
}

impl DomainError {
    pub fn new(kind: DomainErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewUserRequest {
    pub email: String,
    pub display_name: String,
}

/// Transport-agnostic users domain contract.
#[async_trait]
pub trait UsersServiceContract: Send + Sync {
    async fn get_user(&self, id: &str) -> Result<UserRecord, DomainError>;
    async fn create_user(&self, request: NewUserRequest) -> Result<UserRecord, DomainError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in_seconds: u64,
}

/// Transport-agnostic auth domain contract.
#[async_trait]
pub trait AuthServiceContract: Send + Sync {
    async fn issue_token(&self, user_id: &str) -> Result<SessionToken, DomainError>;
    async fn verify_token(&self, token: &str) -> Result<String, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topology_parse_is_case_insensitive_and_compatible() {
        assert_eq!(
            ServiceTopology::parse("monolith"),
            Some(ServiceTopology::Monolith)
        );
        assert_eq!(
            ServiceTopology::parse("SINGLE_SERVICE"),
            Some(ServiceTopology::Monolith)
        );
        assert_eq!(
            ServiceTopology::parse("distributed"),
            Some(ServiceTopology::Distributed)
        );
        assert_eq!(
            ServiceTopology::parse("split_services"),
            Some(ServiceTopology::Distributed)
        );
        assert_eq!(ServiceTopology::parse("unknown"), None);
    }

    // Serialized: mutates process-global env vars, which races the other
    // `#[serial]` env-reading suites when run concurrently.
    #[test]
    #[serial_test::serial]
    fn topology_runtime_reads_endpoint_map() {
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "distributed");
        std::env::set_var(
            "KRAB_RUNTIME_ENDPOINTS_JSON",
            r#"{"users":{"base_url":"http://127.0.0.1:3002","timeout_ms":1200,"max_retries":1}}"#,
        );

        let runtime = TopologyRuntime::from_env();
        assert_eq!(runtime.mode, ServiceTopology::Distributed);
        let users = runtime
            .endpoint_for("users")
            .expect("users endpoint missing");
        assert_eq!(users.base_url, "http://127.0.0.1:3002");
        assert_eq!(users.timeout_ms, 1200);
        assert_eq!(users.max_retries, 1);

        std::env::remove_var("KRAB_RUNTIME_TOPOLOGY");
        std::env::remove_var("KRAB_RUNTIME_ENDPOINTS_JSON");
    }
}
