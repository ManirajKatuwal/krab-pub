//! Split-topology reference service.
//!
//! One process, two protocol adapters (REST and GraphQL) over one domain
//! contract — the shape a `krab topology split` service takes before its
//! adapters are moved into separate binaries. The domain is deliberately
//! [`InMemoryDomainService`]: what this service demonstrates is the adapter
//! boundary, not persistence.

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use axum::Router;
use krab_core::config::KrabConfig;
use krab_core::protocol::{DeploymentTopology, ExposureMode, ProtocolConfig, ProtocolKind};
use krab_core::service::{serve_with_graceful_shutdown, ApiService, ServiceConfig};
use krab_core::telemetry::init_tracing;
use std::sync::Arc;
use tracing::warn;

pub mod adapters;
pub mod domain;
mod runtime;

pub use crate::runtime::{build_app, AppState};

use crate::domain::service::{DomainService, InMemoryDomainService};

/// Service identity used when `KRAB_SERVICE_NAME` is unset.
pub const DEFAULT_SERVICE_NAME: &str = "users-split";

/// Bind port used when `KRAB_PORT` is unset. Matches `krab.toml`.
pub const DEFAULT_PORT: u16 = 3207;

/// This service's identity, resolved the way `KrabConfig::from_env_checked`
/// resolves it: `KRAB_SERVICE_NAME` if set, else [`DEFAULT_SERVICE_NAME`].
///
/// `build_default_app` needs the name before it has a `KrabConfig` — it builds
/// the router without loading one — and the two must not be able to disagree,
/// because the name selects the `KRAB_PROTOCOL_ENABLED_<NAME>` override that
/// decides which adapters get mounted.
fn resolved_service_name() -> String {
    std::env::var("KRAB_SERVICE_NAME").unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_string())
}

struct UsersSplitService {
    config: ServiceConfig,
    domain: Arc<dyn DomainService>,
}

#[async_trait]
impl ApiService for UsersSplitService {
    async fn start(&self) -> Result<()> {
        let protocol_config = self
            .config
            .protocol
            .clone()
            .unwrap_or_else(ProtocolConfig::from_env);
        let state = AppState::try_new(self.domain.clone(), protocol_config)?;

        serve_with_graceful_shutdown(build_app(state), &self.config).await
    }
}

/// The env key `ProtocolConfig::from_env` reads for a service-local protocol
/// override, derived the same way the framework derives it.
fn service_local_protocol_env_key(service_name: &str) -> String {
    let suffix: String = service_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("KRAB_PROTOCOL_ENABLED_{suffix}")
}

/// Parse a `rest,graphql,rpc` list the way `ProtocolConfig::from_env` parses
/// the same env values: unknown entries are skipped, duplicates collapse, and
/// nothing recognisable means no override.
fn parse_protocol_list(raw: &str) -> Vec<ProtocolKind> {
    let mut parsed = Vec::new();
    for entry in raw.split(',') {
        if let Some(kind) = ProtocolKind::parse(entry) {
            if !parsed.contains(&kind) {
                parsed.push(kind);
            }
        }
    }
    parsed
}

/// The service-local protocol override, read under *this service's* name.
///
/// `ProtocolConfig::from_env` derives the `KRAB_PROTOCOL_ENABLED_<NAME>` key
/// from `KRAB_SERVICE_NAME`, then `KRAB_SERVICE`, then the literal `service` —
/// never from the crate's own default name. So running this binary directly,
/// which the README documents and which leaves `KRAB_SERVICE_NAME` unset, the
/// framework looks for `KRAB_PROTOCOL_ENABLED_SERVICE` and never sees
/// `KRAB_PROTOCOL_ENABLED_USERS_SPLIT` — the key this service advertises. The
/// override was inert unless the environment happened to name the service too.
fn service_local_protocols(service_name: &str) -> Option<Vec<ProtocolKind>> {
    std::env::var(service_local_protocol_env_key(service_name))
        .ok()
        .map(|raw| parse_protocol_list(&raw))
        .filter(|parsed| !parsed.is_empty())
}

/// Resolve protocol exposure for this service.
///
/// The framework default is REST-only, which would answer `/api/v1/graphql`
/// with `PROTOCOL_NOT_SUPPORTED`. Serving both adapters over one domain is the
/// whole point of this service, so both are enabled when nothing in the
/// environment says otherwise — the same backward-compatible default
/// `service_users` applies.
fn resolve_protocol_config(service_name: &str) -> Result<ProtocolConfig> {
    let service_local = service_local_protocols(service_name);
    let mut protocol_config = ProtocolConfig::from_env();

    if std::env::var_os("KRAB_PROTOCOL_EXPOSURE_MODE").is_none()
        && std::env::var_os("KRAB_PROTOCOL_ENABLED").is_none()
        && service_local.is_none()
    {
        protocol_config.exposure_mode = ExposureMode::Multi;
        protocol_config.enabled_protocols = vec![ProtocolKind::Rest, ProtocolKind::Graphql];
        protocol_config.default_protocol = ProtocolKind::Rest;
        // One binary serves both adapters today; `split_services` would claim
        // a per-protocol process that does not exist.
        protocol_config.topology = DeploymentTopology::SingleService;
    } else if let Some(enabled) = service_local {
        // Applied here rather than left to `from_env`, for the key-derivation
        // reason in `service_local_protocols`. Precedence matches the
        // framework's: a service-local list wins over `KRAB_PROTOCOL_ENABLED`,
        // and the default protocol is always present in the enabled set.
        protocol_config.enabled_protocols = enabled;
        if !protocol_config
            .enabled_protocols
            .contains(&protocol_config.default_protocol)
        {
            protocol_config
                .enabled_protocols
                .push(protocol_config.default_protocol);
        }
    }

    protocol_config
        .validate()
        .map_err(|errs| anyhow::anyhow!("invalid protocol configuration: {}", errs.join("; ")))?;

    Ok(protocol_config)
}

fn bootstrap_users_split_service() -> Result<UsersSplitService> {
    let cfg = KrabConfig::from_env_checked(DEFAULT_SERVICE_NAME, DEFAULT_PORT)
        .context("failed to load users_split config from environment")?;
    let secrets_report = cfg
        .validate_all()
        .context("startup config validation failed")?;
    if !secrets_report.is_clean() {
        warn!(
            issue_count = secrets_report.issues.len(),
            "startup_secrets_policy_warnings_detected"
        );
    }

    let protocol_config = resolve_protocol_config(&cfg.service_name)?;

    Ok(UsersSplitService {
        config: ServiceConfig {
            name: cfg.service_name.clone(),
            host: cfg.host.clone(),
            port: cfg.port,
            protocol: Some(protocol_config),
        },
        domain: InMemoryDomainService::shared(),
    })
}

/// Build the application exactly as [`run_default`] does, without binding a
/// listener.
///
/// Tests use this so they exercise the same governance layers a real request
/// meets — including the auth middleware that inserts `AuthContext`.
pub fn build_default_app(domain: Arc<dyn DomainService>) -> Result<Router> {
    let protocol_config = resolve_protocol_config(&resolved_service_name())?;

    Ok(build_app(AppState::try_new(domain, protocol_config)?))
}

pub async fn run_default() -> Result<()> {
    init_tracing(DEFAULT_SERVICE_NAME);
    let service = bootstrap_users_split_service()?;
    service.start().await
}
