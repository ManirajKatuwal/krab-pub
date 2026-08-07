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

/// Poll running children for unexpected exits and apply restart policy when configured.
pub(super) async fn supervise_exited_children(
    config: &KrabConfig,
    children: &mut HashMap<String, tokio::process::Child>,
    restart_attempts: &mut HashMap<String, u32>,
) {
    let names: Vec<String> = children.keys().cloned().collect();
    for name in names {
        let Some(child) = children.get_mut(&name) else {
            continue;
        };

        match child.try_wait() {
            Ok(Some(status)) => {
                warn!(service = %name, status = %status, code = ?status.code(), "service_exited");
                let service = match config.services.get(&name) {
                    Some(service) => service,
                    None => continue,
                };

                if !service.effective_restart_on_exit() {
                    continue;
                }

                let attempts = restart_attempts.get(&name).copied().unwrap_or(0);
                if attempts >= service.effective_max_restart_attempts() {
                    error!(
                        service = %name,
                        attempts,
                        max_attempts = service.effective_max_restart_attempts(),
                        "service_restart_limit_reached"
                    );
                    continue;
                }

                tokio::time::sleep(Duration::from_millis(
                    service.effective_restart_backoff_ms(),
                ))
                .await;
                match spawn_service_and_wait_ready(&name, service, "automatic restart").await {
                    Ok(new_child) => {
                        children.insert(name.clone(), new_child);
                        let next_attempt = attempts + 1;
                        restart_attempts.insert(name.clone(), next_attempt);
                        info!(service = %name, attempt = next_attempt, "service_auto_restarted");
                    }
                    Err(err) => {
                        restart_attempts.insert(name.clone(), attempts + 1);
                        error!(service = %name, error = %err, "service_auto_restart_failed");
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                error!(service = %name, error = %err, "service_try_wait_failed");
            }
        }
    }
}

/// Shut down all currently running supervised children using per-service shutdown budgets.
pub(super) async fn shutdown_children(
    config: &KrabConfig,
    children: &mut HashMap<String, tokio::process::Child>,
) {
    let names: Vec<String> = children.keys().cloned().collect();
    for name in names {
        let Some(child) = children.get_mut(&name) else {
            continue;
        };
        let timeout_ms = config
            .services
            .get(&name)
            .map(|svc| svc.shutdown_timeout_ms)
            .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS);
        terminate_child(name.as_str(), child, Duration::from_millis(timeout_ms)).await;
    }
    children.clear();
}

/// Restart only services marked as watch-enabled after source changes are detected.
pub(super) async fn restart_watched_services(
    config: &KrabConfig,
    children: &mut HashMap<String, tokio::process::Child>,
    restart_attempts: &mut HashMap<String, u32>,
) {
    let watched_names: Vec<String> = config
        .services
        .iter()
        .filter_map(|(name, service)| {
            if service.watch {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    for name in &watched_names {
        let Some(child) = children.get_mut(name) else {
            continue;
        };
        let timeout_ms = config
            .services
            .get(name)
            .map(|svc| svc.shutdown_timeout_ms)
            .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MS);
        terminate_child(name.as_str(), child, Duration::from_millis(timeout_ms)).await;
    }

    for name in watched_names {
        let Some(service) = config.services.get(&name) else {
            continue;
        };

        match spawn_service_and_wait_ready(&name, service, "watch restart").await {
            Ok(child) => {
                children.insert(name.clone(), child);
                restart_attempts.insert(name.clone(), 0);
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
        .stderr(Stdio::piped());
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
    if let Err(err) = wait_for_service_health(name, service).await {
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
/// A small circuit breaker is used to prevent hammering obviously unavailable services during
/// bootstrap or automatic restart flows.
pub(super) async fn wait_for_service_health(name: &str, service: &ServiceDefinition) -> Result<()> {
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
