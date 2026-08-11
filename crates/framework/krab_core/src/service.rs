use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[cfg(feature = "rest")]
use anyhow::Context as _;
#[cfg(feature = "rest")]
use std::net::SocketAddr;

#[async_trait]
pub trait ApiService: Send + Sync {
    async fn start(&self) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Protocol configuration. `None` keeps legacy single-protocol behavior.
    #[serde(default)]
    pub protocol: Option<crate::protocol::ProtocolConfig>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            name: "unknown".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            protocol: None,
        }
    }
}

#[cfg(feature = "rest")]
pub async fn serve_with_graceful_shutdown(app: axum::Router, config: &ServiceConfig) -> Result<()> {
    let addr = format!("{}:{}", config.host, config.port)
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid {} service bind address", config.name))?;
    let service_name_for_signal = config.name.clone();

    tracing::info!(
        service = %config.name,
        host = %config.host,
        port = config.port,
        %addr,
        "service_listening"
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {} service listener", config.name))?;

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!(service = %service_name_for_signal, "service_shutdown_signal_received");
    })
    .await
    .with_context(|| format!("{} service server exited with error", config.name))?;

    tracing::info!(service = %config.name, "service_shutdown_complete");
    Ok(())
}
