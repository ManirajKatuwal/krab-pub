use config::Config;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

pub(super) const DEFAULT_POLL_MS: u64 = 1000;
pub(super) const DEFAULT_SETTLE_MS: u64 = 500;
pub(super) const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5000;

/// Floor for the polling-fallback interval.
///
/// `poll_ms = 0` used to mean "sleep for zero milliseconds, then walk every
/// watched source tree again" — a hot loop that pins a core and hammers the
/// filesystem. `settle_ms` already had a floor; this gives `poll_ms` one too.
pub(super) const MIN_POLL_MS: u64 = 50;

/// Floor for the post-event settle window, for the same reason as
/// [`MIN_POLL_MS`]: a zero settle restarts services on the first write of a
/// multi-file save instead of coalescing the burst.
pub(super) const MIN_SETTLE_MS: u64 = 50;

#[derive(Debug, Deserialize)]
pub(super) struct KrabConfig {
    pub(super) services: HashMap<String, ServiceDefinition>,
    #[serde(default)]
    pub(super) watch: Option<WatchConfig>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ServiceDefinition {
    pub(super) command: String,
    #[serde(default)]
    pub(super) args: Vec<String>,
    #[serde(default)]
    pub(super) env: HashMap<String, String>,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) watch: bool,
    #[serde(default = "default_true")]
    pub(super) restart_on_exit: bool,
    #[serde(default = "default_restart_backoff_ms")]
    pub(super) restart_backoff_ms: u64,
    #[serde(default = "default_max_restart_attempts")]
    pub(super) max_restart_attempts: u32,
    #[serde(default)]
    pub(super) healthcheck_url: Option<String>,
    #[serde(default = "default_healthcheck_timeout_ms")]
    pub(super) healthcheck_timeout_ms: u64,
    #[serde(default = "default_shutdown_timeout_ms")]
    pub(super) shutdown_timeout_ms: u64,
    #[serde(default)]
    pub(super) depends_on: Vec<String>,
    #[serde(default)]
    pub(super) startup_dependencies: Vec<String>,
    #[serde(default)]
    pub(super) restart_policy: Option<RestartPolicyConfig>,
    #[serde(default)]
    pub(super) healthcheck: Option<HealthProbeConfig>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RestartPolicyConfig {
    #[serde(default = "default_true")]
    pub(super) on_exit: bool,
    #[serde(default = "default_restart_backoff_ms")]
    pub(super) backoff_ms: u64,
    #[serde(default = "default_max_restart_attempts")]
    pub(super) max_attempts: u32,
    /// How long a service must stay up before its earlier crashes stop
    /// counting against `max_attempts`.
    ///
    /// Without this the attempt counter only ever grows, so a service that
    /// crashes once a week is permanently dead after `max_attempts` weeks —
    /// the budget is meant to stop a crash loop, not to cap a process's
    /// lifetime failures.
    #[serde(default = "default_restart_stability_window_ms")]
    pub(super) stability_window_ms: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct HealthProbeConfig {
    pub(super) url: String,
    #[serde(default = "default_healthcheck_timeout_ms")]
    pub(super) timeout_ms: u64,
    #[serde(default = "default_healthcheck_retries")]
    pub(super) retries: u8,
    #[serde(default = "default_healthcheck_interval_ms")]
    pub(super) interval_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct WatchConfig {
    #[serde(default)]
    pub(super) enabled: bool,
    #[serde(default = "default_poll_ms")]
    pub(super) poll_ms: u64,
    #[serde(default = "default_settle_ms")]
    pub(super) settle_ms: u64,
    #[serde(default)]
    pub(super) paths: Vec<String>,
}

impl WatchConfig {
    /// Polling-fallback interval, floored so a `poll_ms = 0` config cannot turn
    /// the fallback loop into a filesystem-scanning spin.
    pub(super) fn effective_poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_ms.max(MIN_POLL_MS))
    }

    /// Quiet period after the last filesystem event before services restart,
    /// floored so a burst of editor writes still coalesces into one restart.
    pub(super) fn effective_settle(&self) -> Duration {
        Duration::from_millis(self.settle_ms.max(MIN_SETTLE_MS))
    }
}

pub(super) fn load_krab_config() -> anyhow::Result<KrabConfig> {
    let settings = Config::builder()
        .add_source(config::File::with_name("krab"))
        .build()?;
    Ok(settings.try_deserialize::<KrabConfig>()?)
}

fn default_poll_ms() -> u64 {
    DEFAULT_POLL_MS
}

fn default_settle_ms() -> u64 {
    DEFAULT_SETTLE_MS
}

fn default_true() -> bool {
    true
}

fn default_restart_backoff_ms() -> u64 {
    500
}

fn default_max_restart_attempts() -> u32 {
    5
}

fn default_restart_stability_window_ms() -> u64 {
    60_000
}

fn default_healthcheck_timeout_ms() -> u64 {
    1200
}

fn default_shutdown_timeout_ms() -> u64 {
    DEFAULT_SHUTDOWN_TIMEOUT_MS
}

fn default_healthcheck_retries() -> u8 {
    10
}

fn default_healthcheck_interval_ms() -> u64 {
    250
}

impl ServiceDefinition {
    pub(super) fn effective_restart_on_exit(&self) -> bool {
        self.restart_policy
            .as_ref()
            .map(|p| p.on_exit)
            .unwrap_or(self.restart_on_exit)
    }

    pub(super) fn effective_restart_backoff_ms(&self) -> u64 {
        self.restart_policy
            .as_ref()
            .map(|p| p.backoff_ms)
            .unwrap_or(self.restart_backoff_ms)
    }

    pub(super) fn effective_max_restart_attempts(&self) -> u32 {
        self.restart_policy
            .as_ref()
            .map(|p| p.max_attempts)
            .unwrap_or(self.max_restart_attempts)
    }

    pub(super) fn effective_restart_stability_window(&self) -> Duration {
        Duration::from_millis(
            self.restart_policy
                .as_ref()
                .map(|p| p.stability_window_ms)
                .unwrap_or_else(default_restart_stability_window_ms),
        )
    }

    pub(super) fn effective_healthcheck_url(&self) -> Option<&str> {
        self.healthcheck
            .as_ref()
            .map(|h| h.url.as_str())
            .or(self.healthcheck_url.as_deref())
    }

    pub(super) fn effective_healthcheck_timeout_ms(&self) -> u64 {
        self.healthcheck
            .as_ref()
            .map(|h| h.timeout_ms)
            .unwrap_or(self.healthcheck_timeout_ms)
    }

    pub(super) fn effective_healthcheck_retries(&self) -> u8 {
        self.healthcheck
            .as_ref()
            .map(|h| h.retries)
            .unwrap_or(default_healthcheck_retries())
            .max(1)
    }

    pub(super) fn effective_healthcheck_interval_ms(&self) -> u64 {
        self.healthcheck
            .as_ref()
            .map(|h| h.interval_ms)
            .unwrap_or(default_healthcheck_interval_ms())
    }

    pub(super) fn effective_startup_deadline_ms(&self) -> u64 {
        let retries = u64::from(self.effective_healthcheck_retries());
        let request_budget = retries.saturating_mul(self.effective_healthcheck_timeout_ms());
        let retry_gaps = u64::from(self.effective_healthcheck_retries().saturating_sub(1))
            .saturating_mul(self.effective_healthcheck_interval_ms());
        request_budget
            .saturating_add(retry_gaps)
            .max(self.effective_healthcheck_timeout_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HealthProbeConfig, RestartPolicyConfig, ServiceDefinition, WatchConfig, DEFAULT_POLL_MS,
        DEFAULT_SETTLE_MS,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    fn sample_service() -> ServiceDefinition {
        ServiceDefinition {
            command: "cargo".to_string(),
            args: vec!["run".to_string()],
            env: HashMap::new(),
            cwd: None,
            watch: false,
            restart_on_exit: true,
            restart_backoff_ms: 500,
            max_restart_attempts: 5,
            healthcheck_url: Some("http://127.0.0.1:3000/ready".to_string()),
            healthcheck_timeout_ms: 1200,
            shutdown_timeout_ms: 5000,
            depends_on: vec![],
            startup_dependencies: vec![],
            restart_policy: None,
            healthcheck: None,
        }
    }

    #[test]
    fn effective_healthcheck_retries_never_returns_zero() {
        let mut service = sample_service();
        service.healthcheck = Some(HealthProbeConfig {
            url: "http://127.0.0.1:3000/ready".to_string(),
            timeout_ms: 800,
            retries: 0,
            interval_ms: 150,
        });

        assert_eq!(service.effective_healthcheck_retries(), 1);
    }

    #[test]
    fn effective_startup_deadline_uses_probe_budget() {
        let mut service = sample_service();
        service.healthcheck = Some(HealthProbeConfig {
            url: "http://127.0.0.1:3000/ready".to_string(),
            timeout_ms: 600,
            retries: 4,
            interval_ms: 100,
        });

        assert_eq!(service.effective_startup_deadline_ms(), 2700);
    }

    #[test]
    fn restart_stability_window_defaults_when_policy_omits_it() {
        let service = sample_service();

        assert_eq!(
            service.effective_restart_stability_window(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn restart_stability_window_honours_explicit_policy() {
        let mut service = sample_service();
        service.restart_policy = Some(RestartPolicyConfig {
            on_exit: true,
            backoff_ms: 700,
            max_attempts: 8,
            stability_window_ms: 5_000,
        });

        assert_eq!(
            service.effective_restart_stability_window(),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn zero_poll_and_settle_are_floored_instead_of_spinning() {
        let watch = WatchConfig {
            enabled: true,
            poll_ms: 0,
            settle_ms: 0,
            paths: vec![],
        };

        assert_eq!(watch.effective_poll_interval(), Duration::from_millis(50));
        assert_eq!(watch.effective_settle(), Duration::from_millis(50));
    }

    #[test]
    fn configured_poll_and_settle_above_the_floor_are_preserved() {
        let watch = WatchConfig {
            enabled: true,
            poll_ms: DEFAULT_POLL_MS,
            settle_ms: DEFAULT_SETTLE_MS,
            paths: vec![],
        };

        assert_eq!(watch.effective_poll_interval(), Duration::from_millis(1000));
        assert_eq!(watch.effective_settle(), Duration::from_millis(500));
    }
}
