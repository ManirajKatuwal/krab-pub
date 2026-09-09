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

use crate::configuration::{
    KrabConfig, ServiceDefinition, DEFAULT_SHUTDOWN_TIMEOUT_MS, PORT_ENV_KEY,
};

const ORCHESTRATOR_ARTIFACT_ROOT: &str = "internal/audit/orchestrator";
const PROBE_BODY_EXCERPT_LIMIT: usize = 160;
/// How long to wait for a child to be reaped after the force-kill path has run.
///
/// Every kill there is best-effort, so this is the bound that keeps a child the
/// system refused to kill from hanging the entire shutdown sequence.
const FORCE_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);

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
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if let Some(pid_u32) = child.id() {
            // The group, not the pid. `krab.toml` spawns services as
            // `cargo run --bin X`, so the direct child is cargo and the service
            // is a grandchild; cargo does not forward signals, so signalling
            // the pid alone killed cargo and left the service running with its
            // port still bound. `spawn_service` makes the child its own group
            // leader, so its pgid equals its pid and this reaches both.
            let pgid = Pid::from_raw(pid_u32 as i32);
            if let Err(err) = killpg(pgid, Signal::SIGTERM) {
                warn!(service = %name, pgid = pid_u32, error = %err, "service_sigterm_failed");
            } else {
                info!(service = %name, pgid = pid_u32, "service_sigterm_sent");
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

    // Windows has neither process groups nor signals, and `Child::kill` reaches
    // only the direct child — `cargo`, which does not pass the kill on to the
    // service binary. The service would survive the orchestrator with its port
    // still bound, and since `9f2e92b` the orchestrator owns ports, so the next
    // run fails to bind. `taskkill /T` walks the tree instead.
    #[cfg(windows)]
    {
        let mut tree_killed = false;

        if let Some(pid) = child.id() {
            match tokio::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .output()
                .await
            {
                Ok(out) if out.status.success() => {
                    info!(service = %name, pid, "service_process_tree_killed");
                    tree_killed = true;
                }
                Ok(out) => {
                    warn!(
                        service = %name,
                        pid,
                        code = ?out.status.code(),
                        stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                        "service_process_tree_kill_reported_failure"
                    );
                }
                Err(err) => {
                    warn!(service = %name, pid, error = %err, "service_process_tree_kill_failed_to_run");
                }
            }
        }

        // `taskkill` is not guaranteed to be on PATH, and it reports failure for
        // a tree it cannot open. Without this fallback the child stays live and
        // the unbounded `wait` below never returns — shutdown hangs rather than
        // leaking. `kill` reaches only `cargo`, so the grandchild can still
        // survive; that is a leak, and a leak beats a hang.
        if !tree_killed {
            if let Err(err) = child.kill().await {
                warn!(service = %name, error = %err, "service_force_kill_failed");
            } else {
                warn!(service = %name, "service_force_killed_direct_child_only_tree_may_survive");
            }
        }
    }

    // The force path has the same problem the graceful path does: `Child::kill`
    // signals `cargo`, not the service it spawned. Signal the group, exactly as
    // the SIGTERM above does, or a service that outlived the shutdown budget is
    // left running with its port bound — the leak this whole function exists to
    // prevent, reappearing at the one moment it matters most.
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if let Some(pid_u32) = child.id() {
            let pgid = Pid::from_raw(pid_u32 as i32);
            if let Err(err) = killpg(pgid, Signal::SIGKILL) {
                warn!(service = %name, pgid = pid_u32, error = %err, "service_group_force_kill_failed");
            } else {
                info!(service = %name, pgid = pid_u32, "service_group_force_killed");
            }
        }

        // The group signal does not reap the direct child; this does, so the
        // `wait` below returns promptly instead of on the timeout.
        if let Err(err) = child.kill().await {
            warn!(service = %name, error = %err, "service_force_kill_failed");
        }
    }

    #[cfg(not(any(unix, windows)))]
    if let Err(err) = child.kill().await {
        warn!(service = %name, error = %err, "service_force_kill_failed");
    }

    // Bounded. Every kill above is best-effort — `taskkill` may be missing, the
    // group may be gone, the child may be unkillable — and an unbounded wait on
    // a live child hangs the whole shutdown, which is worse than reporting the
    // service as unreaped and moving on to the next one.
    match tokio::time::timeout(FORCE_KILL_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            info!(service = %name, status = %status, code = ?status.code(), "service_stopped_forcefully");
        }
        Ok(Err(err)) => {
            warn!(service = %name, error = %err, "service_wait_failed_after_force_kill");
        }
        Err(_) => {
            warn!(
                service = %name,
                timeout_ms = FORCE_KILL_REAP_TIMEOUT.as_millis() as u64,
                "service_still_running_after_force_kill_abandoning_wait"
            );
        }
    }
}

/// Build the child command for a service: program, arguments, resolved
/// environment, and working directory.
///
/// Split out of [`spawn_service`] so the environment a child actually receives
/// can be observed in a test without also taking on the log forwarders and the
/// artifact directory.
pub(super) fn build_command(name: &str, service: &ServiceDefinition) -> Command {
    let child_env = service.resolved_env(name);

    // The child inherits this process's environment and the resolved map is
    // merged over it — there is no `env_clear()` — so anything the manifest
    // does not speak for is still ambient. `KRAB_PORT` was the case where that
    // mattered most: one exported value moved every service off its own port
    // and out from under its own health probe.
    if service.port_is_unpinned() {
        if let Ok(ambient) = std::env::var(PORT_ENV_KEY) {
            warn!(
                service = %name,
                ambient_port = %ambient,
                "service_port_unpinned_inheriting_ambient_krab_port"
            );
        }
    }

    let mut cmd = Command::new(&service.command);
    cmd.args(&service.args).envs(&child_env);
    if let Some(cwd) = &service.cwd {
        cmd.current_dir(cwd);
    }
    cmd
}

/// Spawn a configured service process using its command, arguments, environment, and cwd.
pub(super) async fn spawn_service(
    name: &str,
    service: &ServiceDefinition,
) -> Result<tokio::process::Child> {
    let mut cmd = build_command(name, service);

    // Give the child its own process group so shutdown can reach the whole
    // tree: the direct child is usually `cargo run`, which spawns the service
    // as a grandchild and does not forward signals to it. As group leader the
    // child's pgid equals its pid, which is what `terminate_child` signals.
    //
    // The children no longer share the terminal's foreground group, so a
    // Ctrl-C at the console reaches only the orchestrator. That is the correct
    // arrangement here — `main` installs a `ctrl_c` handler and shuts the
    // services down in dependency order — and it removes the race where the
    // terminal and the orchestrator both signalled the same processes.
    #[cfg(unix)]
    cmd.process_group(0);

    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Partial backstop for the paths that never reach `shutdown_children`.
        // Be precise about what it buys: `kill_on_drop` signals the direct
        // child only, which is `cargo run`, and cargo does not pass it on. So a
        // dropped handle reaps cargo and can leave the service itself running
        // with its port bound — the same gap the group signalling in
        // `terminate_child` exists to close, and one this cannot close because
        // `Drop` is synchronous and the child map is not reachable from it.
        //
        // What that means in practice: an orchestrator panic or SIGKILL may
        // leak services. `krab bootstrap` shutting down normally does not.
        .kill_on_drop(true);

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
        krab_service_name = %service.effective_service_name(name),
        krab_port = ?service.port,
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
    use super::ProbeFailureDiagnostics;
    use super::{build_command, compact_excerpt, sanitize_artifact_component, Stdio, PORT_ENV_KEY};
    use crate::configuration::{ServiceDefinition, SERVICE_NAME_ENV_KEY};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    /// `set_var`/`remove_var` are process-global, so the tests that stage an
    /// ambient `KRAB_PORT` take a turn rather than racing each other.
    static AMBIENT_ENV: Mutex<()> = Mutex::new(());

    /// Echoes the two identity variables the orchestrator injects, separated by
    /// `|`, using the shell that exists on the platform running the test.
    fn echo_identity_service() -> ServiceDefinition {
        #[cfg(windows)]
        let (command, args) = (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                format!("echo %{SERVICE_NAME_ENV_KEY}%^|%{PORT_ENV_KEY}%"),
            ],
        );
        #[cfg(not(windows))]
        let (command, args) = (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("printf '%s|%s' \"${SERVICE_NAME_ENV_KEY}\" \"${PORT_ENV_KEY}\""),
            ],
        );

        ServiceDefinition {
            command,
            args,
            port: None,
            service_name: None,
            env: HashMap::new(),
            cwd: None,
            watch: false,
            restart_on_exit: false,
            restart_backoff_ms: 500,
            max_restart_attempts: 5,
            healthcheck_url: None,
            healthcheck_timeout_ms: 1200,
            shutdown_timeout_ms: 5000,
            depends_on: vec![],
            startup_dependencies: vec![],
            restart_policy: None,
            healthcheck: None,
        }
    }

    /// Spawn through the real command-building path with `ambient` staged in
    /// this process's environment.
    ///
    /// The child's *inherited* environment is captured at `spawn`, not at
    /// build, so the staging has to survive that call — but no longer: the
    /// lock is released before anything is awaited, both because a
    /// `MutexGuard` may not be held across an await and because the ambient
    /// variables must not outlive the spawn they were staged for.
    fn spawn_with_ambient(
        name: &str,
        service: &ServiceDefinition,
        ambient: &[(&str, &str)],
    ) -> tokio::process::Child {
        let _guard = AMBIENT_ENV.lock().unwrap_or_else(|err| err.into_inner());
        // Restored, not removed: a developer running the suite with KRAB_PORT
        // exported would otherwise lose it for the rest of the test binary,
        // and the next test to stage an ambient value would be measuring a
        // different starting environment than the first one did.
        let previous: Vec<(&str, Option<String>)> = ambient
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();
        for (key, value) in ambient {
            std::env::set_var(key, value);
        }

        let child = build_command(name, service)
            .stdout(Stdio::piped())
            .spawn()
            .expect("child process spawns");

        for (key, value) in &previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        child
    }

    async fn observed_identity(child: tokio::process::Child) -> String {
        let output = child
            .wait_with_output()
            .await
            .expect("child process completes");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn the_spawn_path_injects_the_declared_port_and_name_over_an_ambient_one() {
        // The reproduction, in miniature: an exported `KRAB_PORT` used to reach
        // every child, because the orchestrator merges per-service env over an
        // inherited environment and never spoke for the port itself.
        let mut service = echo_identity_service();
        service.port = Some(3001);

        let child = spawn_with_ambient(
            "auth",
            &service,
            &[(PORT_ENV_KEY, "3000"), (SERVICE_NAME_ENV_KEY, "krab")],
        );
        let observed = observed_identity(child).await;

        assert_eq!(observed, "auth|3001", "child saw {observed}");
    }

    #[tokio::test]
    async fn an_explicit_env_entry_still_wins_over_the_injected_default() {
        let mut service = echo_identity_service();
        service.port = Some(3001);
        service.service_name = Some("auth".to_string());
        service.env.insert(PORT_ENV_KEY.to_string(), "3999".into());
        service
            .env
            .insert(SERVICE_NAME_ENV_KEY.to_string(), "pinned".into());

        let child = spawn_with_ambient("auth", &service, &[(PORT_ENV_KEY, "3000")]);
        let observed = observed_identity(child).await;

        assert_eq!(observed, "pinned|3999", "child saw {observed}");
    }

    #[tokio::test]
    async fn a_service_with_no_declared_port_still_inherits_the_ambient_one() {
        // Unchanged behaviour for manifests that declare no port — the
        // orchestrator invents nothing, it only warns.
        let service = echo_identity_service();

        let child = spawn_with_ambient("frontend", &service, &[(PORT_ENV_KEY, "3000")]);
        let observed = observed_identity(child).await;

        assert_eq!(observed, "frontend|3000", "child saw {observed}");
    }

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
