use config::Config;
use serde::Deserialize;
use std::collections::HashMap;

pub(super) const DEFAULT_POLL_MS: u64 = 1000;
pub(super) const DEFAULT_SETTLE_MS: u64 = 500;
pub(super) const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 5000;

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
    use super::{HealthProbeConfig, ServiceDefinition};
    use std::collections::HashMap;

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
}
