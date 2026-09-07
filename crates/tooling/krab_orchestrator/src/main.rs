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
    restart_watched_services, shutdown_children, spawn_service_and_wait_ready, supervise_children,
    ServiceSupervision,
};
use crate::watch_runtime::{build_event_watch_runtime, watch_fingerprint};

/// How often the supervisor reaps exited children when it is not otherwise
/// woken. Independent of `watch.poll_ms`, which paces filesystem scanning.
const SUPERVISION_TICK: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing("krab_orchestrator");

    info!("Starting Krab Orchestrator...");

    // Propagated rather than logged-and-swallowed. Returning `Ok(())` here made
    // a missing or malformed krab.toml exit 0, so CI, `krab bootstrap`, and any
    // process supervisor above the orchestrator all read a failed start as a
    // clean run.
    let config = load_krab_config().map_err(|err| {
        error!(error = %err, "krab_config_load_failed");
        err.context(
            "failed to load krab.toml; it must exist in the directory the orchestrator is started from, parse as TOML, and declare a service graph whose ports and names are unique",
        )
    })?;

    info!(
        "Loaded configuration for services: {:?}",
        config.services.keys()
    );
    run_supervisor(config).await
}

async fn run_supervisor(config: KrabConfig) -> Result<()> {
    let mut children = HashMap::<String, tokio::process::Child>::new();
    let mut supervision = HashMap::<String, ServiceSupervision>::new();

    let startup_order = resolve_startup_order(&config.services)?;
    info!(order = ?startup_order, "startup_order_resolved");

    for name in &startup_order {
        let Some(service) = config.services.get(name) else {
            continue;
        };
        match spawn_service_and_wait_ready(name, service, "initial startup").await {
            Ok(child) => {
                children.insert(name.clone(), child);
                supervision.insert(name.clone(), ServiceSupervision::started_now());
            }
            Err(err) => {
                error!(service = %name, error = %err, "service_spawn_failed");
                shutdown_children(&config, &startup_order, &mut children).await;
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
                    shutdown_children(&config, &startup_order, &mut children).await;
                    return Ok(());
                }
                _ = tokio::time::sleep(SUPERVISION_TICK) => {
                    supervise_children(&config, &startup_order, &mut children, &mut supervision).await;
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

    let poll_interval = watch_cfg.effective_poll_interval();
    let settle = watch_cfg.effective_settle();

    let mut event_watch = build_event_watch_runtime(&watch_paths)?;
    if event_watch.is_some() {
        info!(
            poll_ms = poll_interval.as_millis() as u64,
            settle_ms = settle.as_millis() as u64,
            paths = ?watch_paths,
            "watch_mode_enabled_event"
        );
    } else {
        info!(
            poll_ms = poll_interval.as_millis() as u64,
            paths = ?watch_paths,
            "watch_mode_enabled_polling_fallback"
        );
    }

    if let Some(runtime) = event_watch.as_mut() {
        let mut pending_restart = false;
        let mut restart_deadline: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("shutdown_signal_received");
                    shutdown_children(&config, &startup_order, &mut children).await;
                    return Ok(());
                }
                _ = tokio::time::sleep(SUPERVISION_TICK) => {
                    supervise_children(&config, &startup_order, &mut children, &mut supervision).await;
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
                    restart_watched_services(&config, &startup_order, &mut children, &mut supervision).await;
                }
            }
        }
    }

    // Reached only when the event watcher could not be built, or its channel
    // closed mid-run. Children are already running by this point, so a failure
    // to take the first fingerprint has to tear them down rather than return
    // and leave them orphaned.
    let mut fingerprint = match watch_fingerprint(&watch_paths) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            error!(error = %err, "watch_fingerprint_initial_scan_failed");
            shutdown_children(&config, &startup_order, &mut children).await;
            return Err(err);
        }
    };

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown_signal_received");
                shutdown_children(&config, &startup_order, &mut children).await;
                return Ok(());
            }
            _ = tokio::time::sleep(poll_interval) => {
                supervise_children(&config, &startup_order, &mut children, &mut supervision).await;

                // A scan error is transient by nature — a build deleting a file
                // between `read_dir` and `metadata` is routine. Skipping the
                // cycle keeps the services up; propagating used to kill the
                // orchestrator and orphan every child it had spawned.
                let current = match watch_fingerprint(&watch_paths) {
                    Ok(current) => current,
                    Err(err) => {
                        warn!(error = %err, "watch_fingerprint_scan_failed_skipping_cycle");
                        continue;
                    }
                };
                if current == fingerprint {
                    continue;
                }

                fingerprint = current;
                info!("Source changes detected. Restarting watched services...");
                restart_watched_services(&config, &startup_order, &mut children, &mut supervision).await;
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

#[cfg(test)]
mod tests {
    use super::resolve_startup_order;
    use crate::configuration::ServiceDefinition;
    use std::collections::HashMap;

    fn service(depends_on: &[&str], startup_dependencies: &[&str]) -> ServiceDefinition {
        ServiceDefinition {
            command: "cargo".to_string(),
            args: vec![],
            port: None,
            service_name: None,
            env: HashMap::new(),
            cwd: None,
            watch: false,
            restart_on_exit: true,
            restart_backoff_ms: 500,
            max_restart_attempts: 5,
            healthcheck_url: None,
            healthcheck_timeout_ms: 1200,
            shutdown_timeout_ms: 5000,
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            startup_dependencies: startup_dependencies.iter().map(|s| s.to_string()).collect(),
            restart_policy: None,
            healthcheck: None,
        }
    }

    fn graph(entries: Vec<(&str, ServiceDefinition)>) -> HashMap<String, ServiceDefinition> {
        entries
            .into_iter()
            .map(|(name, def)| (name.to_string(), def))
            .collect()
    }

    fn position(order: &[String], name: &str) -> usize {
        order
            .iter()
            .position(|candidate| candidate == name)
            .unwrap_or_else(|| panic!("'{name}' missing from resolved order {order:?}"))
    }

    #[test]
    fn dependencies_start_before_their_dependents() {
        let services = graph(vec![
            ("frontend", service(&["auth", "users"], &[])),
            ("users", service(&["auth"], &[])),
            ("auth", service(&[], &[])),
        ]);

        let order = resolve_startup_order(&services).expect("graph is acyclic");

        assert_eq!(order.len(), 3);
        assert!(position(&order, "auth") < position(&order, "users"));
        assert!(position(&order, "users") < position(&order, "frontend"));
    }

    #[test]
    fn startup_dependencies_are_honoured_alongside_depends_on() {
        // `krab.toml` sets both keys for the same edge; neither may be ignored.
        let services = graph(vec![
            ("frontend", service(&[], &["users"])),
            ("users", service(&["auth"], &["auth"])),
            ("auth", service(&[], &[])),
        ]);

        let order = resolve_startup_order(&services).expect("graph is acyclic");

        assert!(position(&order, "auth") < position(&order, "users"));
        assert!(position(&order, "users") < position(&order, "frontend"));
    }

    #[test]
    fn order_is_deterministic_across_runs() {
        // The resolver walks a HashMap, so it sorts its roots. Without that,
        // startup order — and therefore shutdown order — would vary per run.
        let build = || {
            graph(vec![
                ("frontend", service(&["auth"], &[])),
                ("users", service(&["auth"], &[])),
                ("auth", service(&[], &[])),
                ("gateway", service(&["frontend", "users"], &[])),
            ])
        };

        let first = resolve_startup_order(&build()).expect("graph is acyclic");
        for _ in 0..16 {
            assert_eq!(resolve_startup_order(&build()).unwrap(), first);
        }
    }

    #[test]
    fn a_dependency_cycle_is_rejected() {
        let services = graph(vec![
            ("auth", service(&["users"], &[])),
            ("users", service(&["auth"], &[])),
        ]);

        let err = resolve_startup_order(&services).expect_err("cycle must be rejected");

        assert!(
            err.to_string().contains("dependency cycle detected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_service_depending_on_itself_is_rejected() {
        let services = graph(vec![("auth", service(&["auth"], &[]))]);

        let err = resolve_startup_order(&services).expect_err("self-cycle must be rejected");

        assert!(
            err.to_string().contains("dependency cycle detected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_unknown_dependency_is_rejected() {
        let services = graph(vec![("frontend", service(&["nope"], &[]))]);

        let err = resolve_startup_order(&services).expect_err("unknown dep must be rejected");

        assert!(
            err.to_string()
                .contains("depends on unknown service 'nope'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_service_appears_exactly_once_when_two_dependents_share_it() {
        let services = graph(vec![
            ("frontend", service(&["auth"], &[])),
            ("users", service(&["auth"], &[])),
            ("auth", service(&[], &[])),
        ]);

        let order = resolve_startup_order(&services).expect("graph is acyclic");

        assert_eq!(
            order.iter().filter(|name| name.as_str() == "auth").count(),
            1
        );
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn an_empty_service_map_resolves_to_an_empty_order() {
        let order = resolve_startup_order(&HashMap::new()).expect("empty graph is acyclic");

        assert!(order.is_empty());
    }
}
