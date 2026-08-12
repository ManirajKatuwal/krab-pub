use anyhow::{Context, Result};
use krab_core::resilience::CircuitBreaker;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tracing::{error, info, warn};

use crate::configuration::{KrabConfig, ServiceDefinition, DEFAULT_SHUTDOWN_TIMEOUT_MS};

const ORCHESTRATOR_ARTIFACT_ROOT: &str = "internal/audit/orchestrator";
const PROBE_BODY_EXCERPT_LIMIT: usize = 160;

#[derive(Debug, Default)]
struct ProbeFailureDiagnostics {
    attempts: u8,
    blocked_attempts: u8,
    last_status: Option<String>,
    last_error: Option<String>,
    last_body_excerpt: Option<String>,
}

impl ProbeFailureDiagnostics {
    fn describe(
        &self,
        name: &str,
        url: &str,
        retries: u8,
        elapsed: Duration,
        startup_deadline_ms: u64,
    ) -> String {
        let last_status = self.last_status.as_deref().unwrap_or("n/a");
        let last_error = self.last_error.as_deref().unwrap_or("n/a");
        let last_body_excerpt = self.last_body_excerpt.as_deref().unwrap_or("n/a");
        format!(
            "service '{name}' failed readiness probe at {url} after {attempts}/{retries} attempts over {elapsed_ms} ms (startup_deadline_ms={startup_deadline_ms}, blocked_attempts={blocked_attempts}, last_status={last_status}, last_error={last_error}, last_body_excerpt={last_body_excerpt})",
            attempts = self.attempts,
            elapsed_ms = elapsed.as_millis(),
            blocked_attempts = self.blocked_attempts,
        )
    }
}

fn sanitize_artifact_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn orchestrator_artifact_dir() -> &'static Path {
    static ARTIFACT_DIR: OnceLock<PathBuf> = OnceLock::new();
    ARTIFACT_DIR
        .get_or_init(|| {
            let run_id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            PathBuf::from(ORCHESTRATOR_ARTIFACT_ROOT)
                .join(format!("run-{run_id}-{}", std::process::id()))
        })
        .as_path()
}

fn log_artifact_path(service: &str, stream: &str) -> PathBuf {
    orchestrator_artifact_dir().join(format!(
        "{}.{}.log",
        sanitize_artifact_component(service),
        sanitize_artifact_component(stream)
    ))
}

fn log_prefix(service: &str, stream: &str) -> String {
    format!("[{service}::{stream}]")
}

fn compact_excerpt(input: &str, max_len: usize) -> Option<String> {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    if compact.chars().count() <= max_len {
        return Some(compact);
    }
    let truncated: String = compact.chars().take(max_len.saturating_sub(3)).collect();
    Some(format!("{truncated}..."))
}

async fn response_body_excerpt(response: reqwest::Response) -> Option<String> {
    match response.text().await {
        Ok(body) => compact_excerpt(&body, PROBE_BODY_EXCERPT_LIMIT),
        Err(_) => None,
    }
}

/// Per-service restart bookkeeping.
///
/// Kept in its own map rather than alongside the child handle because it has
/// to outlive the child: a crashed service holds no handle while it waits out
/// its backoff, and its attempt count must survive that gap.
#[derive(Debug)]
pub(super) struct ServiceSupervision {
    /// Restarts attempted since the last time the service ran cleanly for a
    /// full stability window.
    attempts: u32,
    /// When the current (or most recent) child was spawned.
    started_at: Instant,
    /// Set while a crashed service waits out its restart backoff. `None` means
    /// the service is running, or its budget is spent and it has been given up
    /// on.
    retry_at: Option<Instant>,
}

impl ServiceSupervision {
    pub(super) fn started_now() -> Self {
        Self {
            attempts: 0,
            started_at: Instant::now(),
            retry_at: None,
        }
    }

    fn mark_started(&mut self) {
        self.started_at = Instant::now();
        self.retry_at = None;
    }
}

/// Reap exited children and start the ones whose backoff has elapsed.
pub(super) async fn supervise_children(
    config: &KrabConfig,
    startup_order: &[String],
    children: &mut HashMap<String, tokio::process::Child>,
    supervision: &mut HashMap<String, ServiceSupervision>,
) {
    reap_exited_children(config, children, supervision);
    spawn_due_restarts(config, startup_order, children, supervision).await;
}

/// Notice children that have exited and schedule their restarts.
///
/// Synchronous on purpose. `try_wait` never blocks, and keeping the backoff out
/// of this function is what stops one crashed service from stalling
/// supervision — and Ctrl-C handling — for every other service.
fn reap_exited_children(
    config: &KrabConfig,
    children: &mut HashMap<String, tokio::process::Child>,
    supervision: &mut HashMap<String, ServiceSupervision>,
) {
    let now = Instant::now();
    let names: Vec<String> = children.keys().cloned().collect();

    for name in names {
        let Some(child) = children.get_mut(&name) else {
            continue;
        };

        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => continue,
            Err(err) => {
                error!(service = %name, error = %err, "service_try_wait_failed");
                continue;
            }
        };

        // Drop the handle as soon as the child is reaped. Tokio caches the exit
        // status (`FusedChild::Done`), so a handle left in this map reports the
        // same exit on every later tick: a service that policy declines to
        // restart used to re-log its own death twice a second, forever.
        children.remove(&name);
        warn!(service = %name, status = %status, code = ?status.code(), "service_exited");

        let Some(service) = config.services.get(&name) else {
            continue;
        };
        let entry = supervision
            .entry(name.clone())
            .or_insert_with(ServiceSupervision::started_now);

        // A service that stayed up for a full stability window before dying is
        // not in a crash loop, so it gets its budget back. Without this the
        // counter only ever grew, and a service that crashed once a week was
        // permanently dead after `max_attempts` weeks.
        let uptime = now.saturating_duration_since(entry.started_at);
        let stability_window = service.effective_restart_stability_window();
        if entry.attempts > 0 && uptime >= stability_window {
            info!(
                service = %name,
                uptime_ms = uptime.as_millis() as u64,
                stability_window_ms = stability_window.as_millis() as u64,
                previous_attempts = entry.attempts,
                "service_restart_budget_reset"
            );
            entry.attempts = 0;
        }

        if !service.effective_restart_on_exit() {
            entry.retry_at = None;
            info!(service = %name, "service_not_restarted_by_policy");
            continue;
        }

        let max_attempts = service.effective_max_restart_attempts();
        if entry.attempts >= max_attempts {
            entry.retry_at = None;
            error!(
                service = %name,
                attempts = entry.attempts,
                max_attempts,
                stability_window_ms = stability_window.as_millis() as u64,
                "service_restart_limit_reached"
            );
            continue;
        }

        let backoff = Duration::from_millis(service.effective_restart_backoff_ms());
        entry.retry_at = Some(now + backoff);
        info!(
            service = %name,
            attempt = entry.attempts + 1,
            max_attempts,
            backoff_ms = backoff.as_millis() as u64,
            "service_restart_scheduled"
        );
    }
}

/// Start services whose restart backoff has elapsed, in dependency order.
async fn spawn_due_restarts(
    config: &KrabConfig,
    startup_order: &[String],
    children: &mut HashMap<String, tokio::process::Child>,
    supervision: &mut HashMap<String, ServiceSupervision>,
) {
    let now = Instant::now();
    let due: Vec<String> = startup_order
        .iter()
        .filter(|name| !children.contains_key(*name))
        .filter(|name| {
            supervision
                .get(*name)
                .and_then(|state| state.retry_at)
                .is_some_and(|at| at <= now)
        })
        .cloned()
        .collect();

    for name in due {
        let Some(service) = config.services.get(&name) else {
            continue;
        };

        let attempt = {
            let Some(entry) = supervision.get_mut(&name) else {
                continue;
            };
            entry.attempts += 1;
            entry.retry_at = None;
            entry.attempts
        };

        let max_attempts = service.effective_max_restart_attempts();
        match spawn_service_and_wait_ready(&name, service, "automatic restart").await {
            Ok(child) => {
                children.insert(name.clone(), child);
                if let Some(entry) = supervision.get_mut(&name) {
                    entry.mark_started();
                }
                info!(service = %name, attempt, max_attempts, "service_auto_restarted");
            }
            Err(err) => {
                error!(service = %name, error = %err, attempt, max_attempts, "service_auto_restart_failed");
                // A failed restart consumes an attempt like any other. Re-arm
                // the backoff so the next tick retries, until the budget runs
                // out — the spawn path has no child to reap, so this is the
                // only place that can notice the budget is spent.
                let Some(entry) = supervision.get_mut(&name) else {
                    continue;
                };
                if attempt < max_attempts {
                    entry.retry_at = Some(
                        Instant::now()
                            + Duration::from_millis(service.effective_restart_backoff_ms()),
                    );
                } else {
                    entry.retry_at = None;
                    error!(
                        service = %name,
                        attempts = attempt,
                        max_attempts,
                        "service_restart_limit_reached"
                    );
                }
            }
        }
    }
}

fn shutdown_timeout_for(config: &KrabConfig, name: &str) -> Duration {
    Duration::from_millis(
        config
            .services
            .get(name)
            .map(|svc| svc.shutdown_timeout_ms)
            .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS),
    )
}

/// Shut down all currently running supervised children using per-service shutdown budgets.
///
/// Stops in reverse dependency order so a dependency outlives everything that
/// talks to it.
pub(super) async fn shutdown_children(
    config: &KrabConfig,
    startup_order: &[String],
    children: &mut HashMap<String, tokio::process::Child>,
) {
    let mut names: Vec<String> = startup_order
        .iter()
        .rev()
        .filter(|name| children.contains_key(*name))
        .cloned()
        .collect();
    // Anything running that the startup order does not mention still has to be
    // stopped; order among those does not matter, only determinism does.
    let mut orphans: Vec<String> = children
        .keys()
        .filter(|name| !startup_order.contains(name))
        .cloned()
        .collect();
    orphans.sort();
    names.extend(orphans);

    for name in names {
        let Some(child) = children.get_mut(&name) else {
            continue;
        };
        terminate_child(name.as_str(), child, shutdown_timeout_for(config, &name)).await;
    }
    children.clear();
}

/// Restart only services marked as watch-enabled after source changes are detected.
pub(super) async fn restart_watched_services(
    config: &KrabConfig,
    startup_order: &[String],
    children: &mut HashMap<String, tokio::process::Child>,
    supervision: &mut HashMap<String, ServiceSupervision>,
) {
    // Driven by the resolved startup order rather than by iterating
    // `config.services`, which is a `HashMap`: restarts used to land in
    // arbitrary order, so the frontend could come back before the auth service
    // it depends on.
    let watched: Vec<String> = startup_order
        .iter()
        .filter(|name| {
            config
                .services
                .get(*name)
                .is_some_and(|service| service.watch)
        })
        .cloned()
        .collect();

    if watched.is_empty() {
        warn!("watch_restart_requested_but_no_service_sets_watch_true");
        return;
    }

    for name in watched.iter().rev() {
        let Some(child) = children.get_mut(name) else {
            continue;
        };
        terminate_child(name.as_str(), child, shutdown_timeout_for(config, name)).await;
        // Drop the handle even though a fresh one usually replaces it below: if
        // the respawn fails, a reaped handle left here would be re-reported as
        // a new exit on every supervision tick.
        children.remove(name);
    }

    for name in watched {
        let Some(service) = config.services.get(&name) else {
            continue;
        };

        match spawn_service_and_wait_ready(&name, service, "watch restart").await {
            Ok(child) => {
                children.insert(name.clone(), child);
                supervision.insert(name.clone(), ServiceSupervision::started_now());
                info!(service = %name, "service_restarted");
            }
            Err(err) => {
                error!(service = %name, error = %err, "service_restart_failed");
            }
        }
    }
}

/// Attempt graceful shutdown first, then forcefully kill the child if the timeout elapses.
pub(super) async fn terminate_child(
    name: &str,
    child: &mut tokio::process::Child,
    timeout: Duration,
) {
    // Nothing to signal or wait for if the process is already gone — without
    // this, tearing down a service that died during its readiness probe burned
    // the full shutdown budget waiting on a corpse.
    if let Ok(Some(status)) = child.try_wait() {
        info!(service = %name, status = %status, code = ?status.code(), "service_already_exited");
        return;
    }

    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        if let Some(pid_u32) = child.id() {
            let pid = Pid::from_raw(pid_u32 as i32);
            if let Err(err) = kill(pid, Signal::SIGTERM) {
                warn!(service = %name, pid = pid_u32, error = %err, "service_sigterm_failed");
            } else {
                info!(service = %name, pid = pid_u32, "service_sigterm_sent");
            }
        }
    }

    #[cfg(not(unix))]
    {
        info!(
            service = %name,
            "graceful_signal_not_supported_on_this_platform_waiting_before_forceful_kill"
        );
    }

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            info!(service = %name, status = %status, code = ?status.code(), "service_stopped_gracefully");
            return;
        }
        Ok(Err(err)) => {
            warn!(service = %name, error = %err, "service_wait_failed_after_shutdown_signal");
        }
        Err(_) => {
            warn!(service = %name, timeout_ms = timeout.as_millis() as u64, "service_shutdown_timeout_elapsed_forcing_kill");
        }
    }

    if let Err(err) = child.kill().await {
        warn!(service = %name, error = %err, "service_force_kill_failed");
        return;
    }

    match child.wait().await {
        Ok(status) => {
            info!(service = %name, status = %status, code = ?status.code(), "service_stopped_forcefully");
        }
        Err(err) => {
            warn!(service = %name, error = %err, "service_wait_failed_after_force_kill");
        }
    }
}

/// Spawn a configured service process using its command, arguments, environment, and cwd.
pub(super) async fn spawn_service(
    name: &str,
    service: &ServiceDefinition,
) -> Result<tokio::process::Child> {
    let mut cmd = Command::new(&service.command);
    cmd.args(&service.args)
        .envs(&service.env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Backstop for the paths that never reach `shutdown_children`: if the
        // orchestrator panics or is killed outright, the runtime still reaps
        // its children instead of leaving every service running and its port
        // bound.
        .kill_on_drop(true);
    if let Some(cwd) = &service.cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "Failed to spawn service '{}' with command '{}' {:?} (cwd: {})",
            name,
            service.command,
            service.args,
            service.cwd.as_deref().unwrap_or(".")
        )
    })?;

    let stdout_log = log_artifact_path(name, "stdout");
    let stderr_log = log_artifact_path(name, "stderr");
    if let Some(stdout) = child.stdout.take() {
        spawn_log_forwarder(name.to_string(), "stdout", stdout, stdout_log.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_log_forwarder(name.to_string(), "stderr", stderr, stderr_log.clone());
    }

    info!(
        service = %name,
        pid = ?child.id(),
        command = %service.command,
        args = ?service.args,
        cwd = ?service.cwd,
        stdout_log = %stdout_log.display(),
        stderr_log = %stderr_log.display(),
        artifact_dir = %orchestrator_artifact_dir().display(),
        "service_started"
    );
    Ok(child)
}

pub(super) async fn spawn_service_and_wait_ready(
    name: &str,
    service: &ServiceDefinition,
    stage: &'static str,
) -> Result<tokio::process::Child> {
    let mut child = spawn_service(name, service).await?;
    if let Err(err) = wait_for_service_health(name, service, &mut child).await {
        terminate_child(
            name,
            &mut child,
            Duration::from_millis(service.shutdown_timeout_ms),
        )
        .await;
        return Err(err.context(format!(
            "service '{}' failed readiness during {}",
            name, stage
        )));
    }
    Ok(child)
}

/// Probe a service health endpoint until it becomes ready or retries are exhausted.
///
/// A small circuit breaker is used to prevent hammering unavailable services during
/// bootstrap or automatic restart flows.
///
/// The child handle is checked between attempts: a service that exits on
/// startup — a bad port binding, a missing migration, a config panic — used to
/// burn the whole retry budget probing a process that had already been dead for
/// seconds, and then report a generic connection error rather than the exit
/// status that actually explains it.
pub(super) async fn wait_for_service_health(
    name: &str,
    service: &ServiceDefinition,
    child: &mut tokio::process::Child,
) -> Result<()> {
    let Some(url) = service.effective_healthcheck_url() else {
        return Ok(());
    };

    let timeout_ms = service.effective_healthcheck_timeout_ms();
    let interval_ms = service.effective_healthcheck_interval_ms();
    let retries = service.effective_healthcheck_retries();
    let startup_deadline_ms = service.effective_startup_deadline_ms();
    let started = Instant::now();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;

    let mut circuit = CircuitBreaker::new(3, Duration::from_secs(2), 1);
    let mut diagnostics = ProbeFailureDiagnostics::default();
    info!(
        service = %name,
        url = %url,
        retries,
        timeout_ms,
        interval_ms,
        startup_deadline_ms,
        "service_health_probe_started"
    );

    for attempt in 1..=retries {
        diagnostics.attempts = attempt;

        match child.try_wait() {
            Ok(Some(status)) => {
                anyhow::bail!(
                    "service '{name}' exited with {status} (code={code:?}) before its readiness probe at {url} succeeded, on attempt {attempt}/{retries} after {elapsed_ms} ms",
                    code = status.code(),
                    elapsed_ms = started.elapsed().as_millis(),
                );
            }
            Ok(None) => {}
            Err(err) => {
                warn!(service = %name, error = %err, "service_try_wait_failed_during_readiness");
            }
        }

        let elapsed_ms = started.elapsed().as_millis() as u64;
        if elapsed_ms > startup_deadline_ms {
            diagnostics.last_error = Some(format!(
                "startup deadline of {startup_deadline_ms} ms elapsed"
            ));
            break;
        }

        if !circuit.allow_request() {
            diagnostics.blocked_attempts += 1;
            diagnostics.last_error = Some("blocked by circuit breaker".to_string());
            warn!(
                service = %name,
                url = %url,
                attempt,
                retries,
                elapsed_ms,
                startup_deadline_ms,
                circuit_state = ?circuit.state(),
                "service_health_probe_blocked_by_circuit"
            );
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            continue;
        }

        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                circuit.record_success();
                info!(
                    service = %name,
                    url = %url,
                    attempt,
                    retries,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    startup_deadline_ms,
                    "service_healthy"
                );
                return Ok(());
            }
            Ok(resp) => {
                circuit.record_failure();
                let status = resp.status();
                let body_excerpt = response_body_excerpt(resp).await;
                diagnostics.last_status = Some(status.to_string());
                diagnostics.last_error = None;
                diagnostics.last_body_excerpt = body_excerpt.clone();
                warn!(
                    service = %name,
                    url = %url,
                    attempt,
                    retries,
                    status = %status,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    startup_deadline_ms,
                    body_excerpt = body_excerpt.as_deref().unwrap_or(""),
                    "service_unhealthy_response"
                );
            }
            Err(err) => {
                circuit.record_failure();
                diagnostics.last_error = Some(err.to_string());
                diagnostics.last_body_excerpt = None;
                warn!(
                    service = %name,
                    url = %url,
                    attempt,
                    retries,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    startup_deadline_ms,
                    error = %err,
                    "service_health_probe_failed"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }

    anyhow::bail!(
        "{}",
        diagnostics.describe(name, url, retries, started.elapsed(), startup_deadline_ms)
    )
}

async fn append_artifact_line(file: &mut tokio::fs::File, line: &str) -> std::io::Result<()> {
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await
}

fn spawn_log_forwarder<R>(service: String, stream: &'static str, reader: R, artifact_path: PathBuf)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let prefix = log_prefix(&service, stream);
        if let Some(parent) = artifact_path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                warn!(
                    service = %service,
                    stream,
                    artifact_path = %artifact_path.display(),
                    error = %err,
                    "service_log_artifact_dir_failed"
                );
            }
        }
        let mut artifact = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&artifact_path)
            .await
        {
            Ok(file) => Some(file),
            Err(err) => {
                warn!(
                    service = %service,
                    stream,
                    artifact_path = %artifact_path.display(),
                    error = %err,
                    "service_log_artifact_open_failed"
                );
                None
            }
        };
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let rendered_line = format!("{prefix} {line}");
                    if let Some(file) = artifact.as_mut() {
                        if let Err(err) = append_artifact_line(file, &rendered_line).await {
                            warn!(
                                service = %service,
                                stream,
                                artifact_path = %artifact_path.display(),
                                error = %err,
                                "service_log_artifact_write_failed"
                            );
                            artifact = None;
                        }
                    }
                    if stream == "stderr" {
                        warn!(
                            service = %service,
                            stream,
                            artifact_path = %artifact_path.display(),
                            line = %rendered_line,
                            "service_log"
                        );
                    } else {
                        info!(
                            service = %service,
                            stream,
                            artifact_path = %artifact_path.display(),
                            line = %rendered_line,
                            "service_log"
                        );
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    warn!(
                        service = %service,
                        stream,
                        artifact_path = %artifact_path.display(),
                        error = %err,
                        "service_log_stream_failed"
                    );
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{compact_excerpt, sanitize_artifact_component, ProbeFailureDiagnostics};
    use std::time::Duration;

    #[test]
    fn compact_excerpt_normalizes_whitespace_and_truncates() {
        let excerpt = compact_excerpt("line one\n\n  line two   line three", 18)
            .expect("excerpt should be generated");

        assert_eq!(excerpt, "line one line t...");
    }

    #[test]
    fn sanitize_artifact_component_replaces_non_portable_chars() {
        assert_eq!(
            sanitize_artifact_component("frontend:stderr/log"),
            "frontend_stderr_log"
        );
    }

    #[test]
    fn probe_failure_description_keeps_last_diagnostics() {
        let diagnostics = ProbeFailureDiagnostics {
            attempts: 4,
            blocked_attempts: 1,
            last_status: Some("503 Service Unavailable".to_string()),
            last_error: Some("connection reset".to_string()),
            last_body_excerpt: Some("warming cache".to_string()),
        };

        let message = diagnostics.describe(
            "frontend",
            "http://127.0.0.1:3000/ready",
            6,
            Duration::from_millis(2200),
            2600,
        );

        assert!(message.contains("after 4/6 attempts"));
        assert!(message.contains("startup_deadline_ms=2600"));
        assert!(message.contains("last_status=503 Service Unavailable"));
        assert!(message.contains("last_error=connection reset"));
        assert!(message.contains("last_body_excerpt=warming cache"));
    }
}
