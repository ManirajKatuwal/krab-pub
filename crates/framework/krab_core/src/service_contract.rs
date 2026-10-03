//! Transport-agnostic service contracts, so one service can call another
//! either in-process or over the network without the caller changing.
//!
//! A contract trait ([`UsersServiceContract`], [`AuthServiceContract`]) is
//! implemented by a local adapter and a remote one; [`TopologyRuntime`]
//! (`KRAB_RUNTIME_TOPOLOGY`, `KRAB_RUNTIME_ENDPOINTS_JSON`) tells the wiring
//! which to use and where remote services live. Failures cross the boundary
//! as [`DomainError`], whatever the transport.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Runtime topology mode used by contract adapter wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTopology {
    /// Every domain is served in-process; wire local adapters. Parsed from
    /// `monolith`, `single` or `single_service`.
    Monolith,
    /// Domains are separate services; wire remote adapters using
    /// [`TopologyRuntime::endpoints`]. Parsed from `distributed`, `split` or
    /// `split_services`.
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
    /// Base URL of the remote service (default `http://127.0.0.1:3000`).
    pub base_url: String,
    /// Per-call timeout, in milliseconds, for the adapter to apply (default
    /// 1500).
    pub timeout_ms: u64,
    /// Retries after the first attempt, for the adapter to apply (default 2).
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
    /// Whether to wire local or remote adapters (default
    /// [`ServiceTopology::Monolith`]).
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
    /// Build topology runtime from environment, tolerating invalid values.
    ///
    /// Consumed env vars:
    /// - KRAB_RUNTIME_TOPOLOGY=monolith|distributed
    /// - KRAB_RUNTIME_ENDPOINTS_JSON={"users":{"base_url":"http://127.0.0.1:3002","timeout_ms":1500,"max_retries":2}}
    ///
    /// Malformed values are swallowed with a `tracing::warn!` and replaced by
    /// defaults. Startup paths should prefer [`TopologyRuntime::from_env_checked`],
    /// which surfaces the same conditions as errors.
    pub fn from_env() -> Self {
        let mut out = Self::default();

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_TOPOLOGY") {
            match ServiceTopology::parse(&raw) {
                Some(mode) => out.mode = mode,
                None => tracing::warn!(
                    raw = %raw.trim(),
                    "krab_runtime_topology_unrecognized_falling_back_to_monolith"
                ),
            }
        }

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_ENDPOINTS_JSON") {
            match serde_json::from_str::<HashMap<String, ServiceEndpoint>>(&raw) {
                Ok(endpoints) => out.endpoints = endpoints,
                Err(error) => tracing::warn!(
                    error = %error,
                    "krab_runtime_endpoints_json_unparseable_ignoring"
                ),
            }
        }

        if out.mode == ServiceTopology::Distributed && out.endpoints.is_empty() {
            tracing::warn!("krab_runtime_topology_distributed_with_empty_endpoint_map");
        }

        out
    }

    /// Build topology runtime from environment, rejecting invalid values.
    ///
    /// Reads the same env vars as [`TopologyRuntime::from_env`] but returns an
    /// error when:
    /// - `KRAB_RUNTIME_TOPOLOGY` is set to an unrecognized value,
    /// - `KRAB_RUNTIME_ENDPOINTS_JSON` is set but does not parse, or
    /// - the resolved mode is `distributed`/`split` with an empty endpoint map.
    pub fn from_env_checked() -> anyhow::Result<Self> {
        let mut out = Self::default();

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_TOPOLOGY") {
            out.mode = ServiceTopology::parse(&raw).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid KRAB_RUNTIME_TOPOLOGY='{}': expected one of monolith|single|single_service|distributed|split|split_services",
                    raw.trim()
                )
            })?;
        }

        if let Ok(raw) = std::env::var("KRAB_RUNTIME_ENDPOINTS_JSON") {
            out.endpoints = serde_json::from_str::<HashMap<String, ServiceEndpoint>>(&raw)
                .map_err(|error| anyhow::anyhow!("invalid KRAB_RUNTIME_ENDPOINTS_JSON: {error}"))?;
        }

        if out.mode == ServiceTopology::Distributed && out.endpoints.is_empty() {
            anyhow::bail!(
                "KRAB_RUNTIME_TOPOLOGY resolves to 'distributed' but the endpoint map is empty; \
                 set KRAB_RUNTIME_ENDPOINTS_JSON with at least one endpoint"
            );
        }

        Ok(out)
    }

    /// The endpoint configured for `domain` (for example `users`), if any.
    pub fn endpoint_for(&self, domain: &str) -> Option<&ServiceEndpoint> {
        self.endpoints.get(domain)
    }
}

/// Stable domain error categories that can be mapped to transport status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainErrorKind {
    /// The input was rejected.
    Validation,
    /// The caller is not authenticated.
    Unauthorized,
    /// The caller is authenticated but not allowed.
    Forbidden,
    /// The requested entity does not exist.
    NotFound,
    /// The request conflicts with existing state.
    Conflict,
    /// A downstream call did not finish in time.
    Timeout,
    /// A downstream service could not be reached or failed.
    UpstreamUnavailable,
    /// Any other failure.
    Internal,
}

/// Transport-agnostic domain error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainError {
    /// The category, which transports map to a status.
    pub kind: DomainErrorKind,
    /// Machine-readable error code.
    pub code: String,
    /// Human-readable description.
    pub message: String,
}

impl DomainError {
    /// A domain error with the given kind, code and message.
    pub fn new(kind: DomainErrorKind, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A user as exchanged across the users contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    /// Stable user id.
    pub id: String,
    /// Email address.
    pub email: String,
    /// Name shown in the UI.
    pub display_name: String,
}

/// Input to [`UsersServiceContract::create_user`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewUserRequest {
    /// Email address of the new user.
    pub email: String,
    /// Name shown in the UI.
    pub display_name: String,
}

/// Transport-agnostic users domain contract.
#[async_trait]
pub trait UsersServiceContract: Send + Sync {
    /// Fetches the user with `id`, under the adapter's own identity — for
    /// service-to-service calls that are not made on behalf of a user.
    async fn get_user(&self, id: &str) -> Result<UserRecord, DomainError>;
    /// Fetches the user with `id` on behalf of the end user whose
    /// `Authorization` header value is `authorization` (`None` when the
    /// request carried none).
    ///
    /// Use this, not [`UsersServiceContract::get_user`], whenever a request
    /// handler serves a caller: a remote adapter must present the caller's
    /// credential to the users service, never its own service identity, or
    /// every caller is answered with data the service account may see (a
    /// confused deputy — `me` resolving to the service account).
    ///
    /// The default ignores `authorization` and calls `get_user`, which is
    /// right for in-process adapters (this process already authenticated the
    /// caller). Remote adapters must override it. Added in 0.6.0.
    async fn get_user_on_behalf_of(
        &self,
        id: &str,
        authorization: Option<&str>,
    ) -> Result<UserRecord, DomainError> {
        let _ = authorization;
        self.get_user(id).await
    }
    /// Creates a user and returns it as stored.
    async fn create_user(&self, request: NewUserRequest) -> Result<UserRecord, DomainError>;
}

/// An issued access token, shaped like an OAuth 2.0 token response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToken {
    /// The token itself.
    pub access_token: String,
    /// How to present it, typically `Bearer`.
    pub token_type: String,
    /// Lifetime from issue, in seconds.
    pub expires_in_seconds: u64,
}

/// Transport-agnostic auth domain contract.
#[async_trait]
pub trait AuthServiceContract: Send + Sync {
    /// Issues an access token for `user_id`.
    async fn issue_token(&self, user_id: &str) -> Result<SessionToken, DomainError>;
    /// Verifies `token` and returns the user id it was issued for.
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

    fn clear_topology_env() {
        std::env::remove_var("KRAB_RUNTIME_TOPOLOGY");
        std::env::remove_var("KRAB_RUNTIME_ENDPOINTS_JSON");
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_accepts_valid_distributed_configuration() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "split");
        std::env::set_var(
            "KRAB_RUNTIME_ENDPOINTS_JSON",
            r#"{"users":{"base_url":"http://127.0.0.1:3002","timeout_ms":1200,"max_retries":1}}"#,
        );

        let runtime = TopologyRuntime::from_env_checked().expect("valid env must parse");
        assert_eq!(runtime.mode, ServiceTopology::Distributed);
        assert!(runtime.endpoint_for("users").is_some());
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_defaults_to_monolith_when_env_is_absent() {
        clear_topology_env();

        let runtime = TopologyRuntime::from_env_checked().expect("absent env means defaults");
        assert_eq!(runtime.mode, ServiceTopology::Monolith);
        assert!(runtime.endpoints.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_rejects_unrecognized_topology() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "mesh");

        let err = TopologyRuntime::from_env_checked()
            .expect_err("unrecognized topology must be rejected")
            .to_string();
        assert!(
            err.contains("invalid KRAB_RUNTIME_TOPOLOGY='mesh'"),
            "{err}"
        );
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_rejects_malformed_endpoints_json() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_ENDPOINTS_JSON", "{not json");

        let err = TopologyRuntime::from_env_checked()
            .expect_err("malformed endpoints JSON must be rejected")
            .to_string();
        assert!(err.contains("invalid KRAB_RUNTIME_ENDPOINTS_JSON"), "{err}");
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_rejects_split_mode_with_empty_endpoint_map() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "split");
        std::env::set_var("KRAB_RUNTIME_ENDPOINTS_JSON", "{}");

        let err = TopologyRuntime::from_env_checked()
            .expect_err("distributed mode with no endpoints must be rejected")
            .to_string();
        assert!(err.contains("endpoint map is empty"), "{err}");
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn from_env_checked_rejects_split_mode_with_no_endpoints_var() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "distributed");

        let err = TopologyRuntime::from_env_checked()
            .expect_err("distributed mode with no endpoint env must be rejected")
            .to_string();
        assert!(err.contains("endpoint map is empty"), "{err}");
        clear_topology_env();
    }

    #[test]
    #[serial_test::serial]
    fn lenient_from_env_swallows_malformed_values_with_defaults() {
        clear_topology_env();
        std::env::set_var("KRAB_RUNTIME_TOPOLOGY", "mesh");
        std::env::set_var("KRAB_RUNTIME_ENDPOINTS_JSON", "{not json");

        let runtime = TopologyRuntime::from_env();
        assert_eq!(runtime.mode, ServiceTopology::Monolith);
        assert!(runtime.endpoints.is_empty());
        clear_topology_env();
    }
}
