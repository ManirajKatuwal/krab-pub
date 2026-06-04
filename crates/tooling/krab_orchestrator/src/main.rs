use anyhow::Result;
use krab_core::telemetry::init_tracing;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

mod configuration;
mod process_runtime;
mod watch_runtime;

use crate::configuration::{
    load_krab_config, KrabConfig, ServiceDefinition, WatchConfig, DEFAULT_POLL_MS,
    DEFAULT_SETTLE_MS,
};
use crate::process_runtime::{
    restart_watched_services, shutdown_children, spawn_service_and_wait_ready,
    supervise_exited_children,
};
use crate::watch_runtime::{build_event_watch_runtime, watch_fingerprint};

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("krab_orchestrator");

    info!("Starting Krab Orchestrator...");

    match load_krab_config() {
        Ok(config) => {
            info!(
                "Loaded configuration for services: {:?}",
                config.services.keys()
            );
            run_supervisor(config).await?;
        }
        Err(e) => {
            error!("Failed to load/parse krab.toml: {}", e);
            info!("Ensure krab.toml exists in the current directory.");
        }
    }

    Ok(())
}

async fn run_supervisor(config: KrabConfig) -> Result<()> {
    let mut children = HashMap::<String, tokio::process::Child>::new();
    let mut restart_attempts = HashMap::<String, u32>::new();

    let startup_order = resolve_startup_order(&config.services)?;
    info!(order = ?startup_order, "startup_order_resolved");

    for name in startup_order {
        let Some(service) = config.services.get(&name) else {
            continue;
        };
        match spawn_service_and_wait_ready(&name, service, "initial startup").await {
            Ok(child) => {
                children.insert(name.clone(), child);
                restart_attempts.insert(name.clone(), 0);
            }
            Err(err) => {
                error!(service = %name, error = %err, "service_spawn_failed");
                shutdown_children(&config, &mut children).await;
                return Err(err);
            }
        }
    }

    let watch_cfg = config.watch.clone().unwrap_or(WatchConfig {
        enabled: false,
        poll_ms: DEFAULT_POLL_MS,
        settle_ms: DEFAULT_SETTLE_MS,
        paths: vec![],
    });

    if !watch_cfg.enabled {
        info!("Watch mode disabled; enabling exit supervision loop.");
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown_signal_received");
                    shutdown_children(&config, &mut children).await;
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    supervise_exited_children(&config, &mut children, &mut restart_attempts).await;
                }
            }
        }
    }

    let watch_paths = if watch_cfg.paths.is_empty() {
        vec![
            "services/service_auth/src".to_string(),
            "services/service_users/src".to_string(),
            "services/service_frontend/src".to_string(),
            "crates/framework/krab_client/src".to_string(),
        ]
    } else {
        watch_cfg.paths.clone()
    };

    let mut event_watch = build_event_watch_runtime(&watch_paths)?;
    if let Some(runtime) = event_watch.as_ref() {
        let _ = runtime;
        info!(
            poll_ms = watch_cfg.poll_ms,
            settle_ms = watch_cfg.settle_ms,
            paths = ?watch_paths,
            "watch_mode_enabled_event"
        );
    } else {
        info!(poll_ms = watch_cfg.poll_ms, paths = ?watch_paths, "watch_mode_enabled_polling_fallback");
    }

    if let Some(runtime) = event_watch.as_mut() {
        let settle = Duration::from_millis(watch_cfg.settle_ms.max(50));
        let mut pending_restart = false;
        let mut restart_deadline: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown_signal_received");
                    shutdown_children(&config, &mut children).await;
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    supervise_exited_children(&config, &mut children, &mut restart_attempts).await;
                }
                maybe_event = runtime.rx.recv() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            info!(kind = ?event.kind, paths = ?event.paths, "watch_event_received");
                            pending_restart = true;
                            restart_deadline = Some(Instant::now() + settle);
                        }
                        Some(Err(err)) => {
                            warn!(error = %err, "watch_event_error");
                        }
                        None => {
                            warn!("watch_event_channel_closed_switching_to_polling_fallback");
                            break;
                        }
                    }
                }
                _ = async {
                    if let Some(deadline) = restart_deadline {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if pending_restart => {
                    pending_restart = false;
                    restart_deadline = None;
                    info!("Source changes detected. Restarting watched services...");
                    restart_watched_services(&config, &mut children, &mut restart_attempts).await;
                }
            }
        }
    }

    let mut fingerprint = watch_fingerprint(&watch_paths)?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown_signal_received");
                shutdown_children(&config, &mut children).await;
                return Ok(());
            }
            _ = tokio::time::sleep(Duration::from_millis(watch_cfg.poll_ms)) => {
                supervise_exited_children(&config, &mut children, &mut restart_attempts).await;

                let current = watch_fingerprint(&watch_paths)?;
                if current == fingerprint {
                    continue;
                }

                fingerprint = current;
                info!("Source changes detected. Restarting watched services...");
                restart_watched_services(&config, &mut children, &mut restart_attempts).await;
            }
        }
    }
}

fn resolve_startup_order(services: &HashMap<String, ServiceDefinition>) -> Result<Vec<String>> {
    fn visit(
        node: &str,
        services: &HashMap<String, ServiceDefinition>,
        temporary: &mut HashMap<String, bool>,
        permanent: &mut HashMap<String, bool>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if permanent.get(node).copied().unwrap_or(false) {
            return Ok(());
        }
        if temporary.get(node).copied().unwrap_or(false) {
            anyhow::bail!("dependency cycle detected at service '{}'", node);
        }

        temporary.insert(node.to_string(), true);
        let service = services
            .get(node)
            .ok_or_else(|| anyhow::anyhow!("service '{}' not found", node))?;

        for dep in service
            .depends_on
            .iter()
            .chain(service.startup_dependencies.iter())
        {
            if !services.contains_key(dep) {
                anyhow::bail!("service '{}' depends on unknown service '{}'", node, dep);
            }
            visit(dep, services, temporary, permanent, order)?;
        }

        temporary.insert(node.to_string(), false);
        permanent.insert(node.to_string(), true);
        if !order.iter().any(|s| s == node) {
            order.push(node.to_string());
        }
        Ok(())
    }

    let mut order = Vec::new();
    let mut temporary = HashMap::new();
    let mut permanent = HashMap::new();

    let mut names: Vec<String> = services.keys().cloned().collect();
    names.sort();
    for name in names {
        visit(&name, services, &mut temporary, &mut permanent, &mut order)?;
    }

    Ok(order)
}
