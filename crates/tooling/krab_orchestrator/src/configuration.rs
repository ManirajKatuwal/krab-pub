use anyhow::Context;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tracing::warn;

/// The variable every Krab service reads for its bind port
/// (`krab_core::config::parse_port_from_env`).
///
/// The per-service `default_port` passed to `KrabConfig::from_env_checked` is a
/// *fallback*, not a floor: when this variable is set in the environment it
/// wins for every service that inherits it. The orchestrator therefore has to
/// set it per child rather than let one ambient value reach all of them.
pub(super) const PORT_ENV_KEY: &str = "KRAB_PORT";

/// The variable every Krab service reads for its own identity.
///
/// Feeds `ServiceConfig.name`, the `service` field on every log line and
/// metric (`krab_core::telemetry`), the `KRAB_PROTOCOL_ENABLED_<NAME>` lookup
/// (`krab_core::protocol`), and migration attribution
/// (`krab_core::db::postgres`). Unlike a wrong port it degrades silently —
/// every service simply reports the same name.
pub(super) const SERVICE_NAME_ENV_KEY: &str = "KRAB_SERVICE_NAME";

/// The orchestrator manifest, read from the directory the orchestrator starts
/// in.
pub(super) const MANIFEST_FILE: &str = "krab.toml";

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
    /// Bind port for this service, injected into the child as `KRAB_PORT`.
    ///
    /// Declaring it here is what makes the topology asserted by
    /// `healthcheck.url` also true of the process behind it. Without it the
    /// child falls back to whatever `KRAB_PORT` it inherits from the
    /// orchestrator's own environment, and a service that moves out from under
    /// its probe fails as a readiness timeout rather than a bind error.
    #[serde(default)]
    pub(super) port: Option<u16>,
    /// Service identity, injected into the child as `KRAB_SERVICE_NAME`.
    ///
    /// Defaults to the `[services.<key>]` table key, which is already the name
    /// the operator gave this service in the manifest. Declare it explicitly
    /// only when the identity the service should report differs from its key.
    #[serde(default)]
    pub(super) service_name: Option<String>,
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
    let raw = std::fs::read_to_string(MANIFEST_FILE)
        .with_context(|| format!("failed to read {MANIFEST_FILE}"))?;
    parse_krab_config(&raw)
}

/// Parse and validate a manifest.
///
/// Uses the `toml` crate directly rather than `config`, which lowercases every
/// key it reads from a file. That silently rewrote `[services.X].env` keys:
/// `RUST_LOG = "info"` reached the child as `rust_log`, and a `KRAB_PORT` or
/// `KRAB_SERVICE_NAME` pinned there reached it as `krab_port` /
/// `krab_service_name`. Windows environment variables are case-insensitive, so
/// the damage was invisible there and total on Linux and macOS — including in
/// containers and CI. `krab doctor` and `krab topology doctor` already read
/// this file with `toml`; the orchestrator now agrees with them about what it
/// says.
pub(super) fn parse_krab_config(raw: &str) -> anyhow::Result<KrabConfig> {
    let config: KrabConfig =
        toml::from_str(raw).with_context(|| format!("failed to parse {MANIFEST_FILE}"))?;
    validate_service_identity(&config)?;
    Ok(config)
}

/// Port written into a `http://host:port/path` health-probe URL, if it carries
/// one explicitly. Deliberately hand-rolled rather than pulling in a URL
/// parser: the only question asked of it is whether an explicit port disagrees
/// with the declared one, and a URL it cannot read simply goes unchecked.
fn probe_url_port(url: &str) -> Option<u16> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // IPv6 literals (`[::1]:3000`) keep their colons inside the brackets.
    let host_port = match authority.rsplit_once(']') {
        Some((_, rest)) => rest.strip_prefix(':')?,
        None => authority.rsplit_once(':').map(|(_, port)| port)?,
    };
    host_port.parse::<u16>().ok()
}

/// Reject manifests whose services cannot all be who they say they are.
///
/// Two services on one port do not fail as a port conflict: the first to bind
/// wins, the second either fails to bind or — worse — the *other* service
/// answers the loser's health probe. Two services under one name collapse into
/// a single identity in logs, metrics, protocol selection, and migration
/// attribution with no error at all. Both are operator errors that only the
/// manifest can see, so they are rejected before anything is spawned.
pub(super) fn validate_service_identity(config: &KrabConfig) -> anyhow::Result<()> {
    let mut names: Vec<&String> = config.services.keys().collect();
    names.sort();

    let mut ports_seen: BTreeMap<u16, &str> = BTreeMap::new();
    let mut identities_seen: BTreeMap<&str, &str> = BTreeMap::new();

    for key in names {
        let service = &config.services[key];

        if let Some(port) = service.port {
            if port == 0 {
                anyhow::bail!(
                    "krab.toml: service '{key}' declares port = 0. Port 0 asks the OS for an \
                     arbitrary free port, which no health probe or sibling service can address; \
                     declare the port the service is meant to bind."
                );
            }
            if let Some(previous) = ports_seen.insert(port, key) {
                anyhow::bail!(
                    "krab.toml: services '{previous}' and '{key}' both declare port = {port}. \
                     The orchestrator injects this as KRAB_PORT, so the two would race for the \
                     same bind address and whichever loses fails its readiness probe — or is \
                     answered by the other service. Give each service its own port."
                );
            }
        }

        let identity = service.effective_service_name(key);
        if let Some(previous) = identities_seen.insert(identity, key) {
            anyhow::bail!(
                "krab.toml: services '{previous}' and '{key}' both resolve to service_name = \
                 '{identity}'. The orchestrator injects this as KRAB_SERVICE_NAME, so their log \
                 lines, metrics, protocol selection, and migration records would be \
                 indistinguishable. Give each service its own name."
            );
        }

        // Not fatal: a probe may legitimately address the service through a
        // proxy or a published container port. It is still nearly always a
        // typo, and silence here is what let the topology drift in the first
        // place.
        if let (Some(port), Some(url)) = (service.port, service.effective_healthcheck_url()) {
            if let Some(probe_port) = probe_url_port(url) {
                if probe_port != port {
                    warn!(
                        service = %key,
                        declared_port = port,
                        probe_port,
                        probe_url = %url,
                        "service_probe_port_differs_from_declared_port"
                    );
                }
            }
        }
    }

    Ok(())
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
    /// Identity this service reports, defaulting to its `[services.<key>]`
    /// table key. A blank `service_name` is treated as absent rather than as
    /// an instruction to run nameless.
    pub(super) fn effective_service_name<'a>(&'a self, key: &'a str) -> &'a str {
        self.service_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(key)
    }

    /// Environment handed to the child, lowest precedence first:
    ///
    /// 1. the orchestrator's own environment, inherited by the child;
    /// 2. the identity the orchestrator injects (`KRAB_SERVICE_NAME`, and
    ///    `KRAB_PORT` when `port` is declared);
    /// 3. explicit `[services.X].env` entries.
    ///
    /// Explicit entries win last so that a manifest can still say something
    /// the structured fields cannot express, and so that pinning a value the
    /// old way keeps working. Everything below them is a default, which is why
    /// an ambient `KRAB_PORT` no longer decides where a service listens.
    pub(super) fn resolved_env(&self, key: &str) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            SERVICE_NAME_ENV_KEY.to_string(),
            self.effective_service_name(key).to_string(),
        );
        if let Some(port) = self.port {
            env.insert(PORT_ENV_KEY.to_string(), port.to_string());
        }
        for (name, value) in &self.env {
            env.insert(name.clone(), value.clone());
        }
        env
    }

    /// True when nothing in the manifest decides this service's port, so it
    /// takes whatever `KRAB_PORT` the orchestrator itself was started with.
    pub(super) fn port_is_unpinned(&self) -> bool {
        self.port.is_none() && !self.env.contains_key(PORT_ENV_KEY)
    }

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
        parse_krab_config, probe_url_port, HealthProbeConfig, RestartPolicyConfig,
        ServiceDefinition, WatchConfig, DEFAULT_POLL_MS, DEFAULT_SETTLE_MS, PORT_ENV_KEY,
        SERVICE_NAME_ENV_KEY,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    fn sample_service() -> ServiceDefinition {
        ServiceDefinition {
            command: "cargo".to_string(),
            args: vec!["run".to_string()],
            port: None,
            service_name: None,
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

    /// The manifest the framework workspace actually ships. If this stops
    /// declaring a distinct port per service, the topology its health probes
    /// assert has drifted from the one its children are told about.
    fn workspace_manifest() -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("krab.toml");
        std::fs::read_to_string(path).ok()
    }

    const TWO_SERVICES: &str = r#"
[services.auth]
command = "cargo"
args = ["run", "--bin", "service_auth"]
port = 3001
env = { RUST_LOG = "info" }

[services.auth.healthcheck]
url = "http://127.0.0.1:3001/ready"

[services.users]
command = "cargo"
args = ["run", "--bin", "service_users"]
port = 3002
service_name = "users-api"
env = { RUST_LOG = "info" }

[services.users.healthcheck]
url = "http://127.0.0.1:3002/ready"
"#;

    #[test]
    fn declared_port_and_name_survive_the_config_loader() {
        // `config` normalises keys from some sources; the whole fix depends on
        // these two reaching the child, so the round trip is asserted rather
        // than assumed.
        let parsed = parse_krab_config(TWO_SERVICES).expect("manifest is valid");

        let auth = &parsed.services["auth"];
        assert_eq!(auth.port, Some(3001));
        assert_eq!(auth.effective_service_name("auth"), "auth");

        let users = &parsed.services["users"];
        assert_eq!(users.port, Some(3002));
        assert_eq!(users.effective_service_name("users"), "users-api");
    }

    #[test]
    fn env_keys_keep_the_case_the_manifest_wrote() {
        // `config` lowercased every key it read, so `RUST_LOG = "info"` was
        // delivered to children as `rust_log` — a no-op everywhere environment
        // variables are case-sensitive, which is everywhere but Windows.
        let parsed = parse_krab_config(TWO_SERVICES).expect("manifest is valid");

        assert_eq!(
            parsed.services["auth"]
                .env
                .get("RUST_LOG")
                .map(String::as_str),
            Some("info")
        );
        assert!(!parsed.services["auth"].env.contains_key("rust_log"));
    }

    #[test]
    fn resolved_env_injects_the_declared_port_and_name() {
        let parsed = parse_krab_config(TWO_SERVICES).expect("manifest is valid");
        let env = parsed.services["auth"].resolved_env("auth");

        assert_eq!(env.get(PORT_ENV_KEY).map(String::as_str), Some("3001"));
        assert_eq!(
            env.get(SERVICE_NAME_ENV_KEY).map(String::as_str),
            Some("auth")
        );
        // Explicit entries are still delivered alongside the injected ones.
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("info"));
    }

    #[test]
    fn service_name_defaults_to_the_manifest_key() {
        let mut service = sample_service();
        service.service_name = None;

        assert_eq!(service.effective_service_name("frontend"), "frontend");
        assert_eq!(
            service
                .resolved_env("frontend")
                .get(SERVICE_NAME_ENV_KEY)
                .map(String::as_str),
            Some("frontend")
        );
    }

    #[test]
    fn a_blank_service_name_falls_back_to_the_key_rather_than_running_nameless() {
        let mut service = sample_service();
        service.service_name = Some("   ".to_string());

        assert_eq!(service.effective_service_name("frontend"), "frontend");
    }

    #[test]
    fn explicit_env_entries_win_over_the_injected_identity() {
        // Documented precedence: inherited environment < injected identity <
        // explicit `[services.X].env`. A manifest that pinned these the old
        // way keeps the behaviour it had.
        let mut service = sample_service();
        service.port = Some(3002);
        service.service_name = Some("users".to_string());
        service.env.insert(PORT_ENV_KEY.to_string(), "3999".into());
        service
            .env
            .insert(SERVICE_NAME_ENV_KEY.to_string(), "override".into());

        let env = service.resolved_env("users");

        assert_eq!(env.get(PORT_ENV_KEY).map(String::as_str), Some("3999"));
        assert_eq!(
            env.get(SERVICE_NAME_ENV_KEY).map(String::as_str),
            Some("override")
        );
    }

    #[test]
    fn a_service_with_no_port_injects_no_port() {
        // Nothing is invented for a manifest that never declared one: the
        // child keeps whatever it inherits, and `port_is_unpinned` is what
        // makes that visible in the log.
        let service = sample_service();

        assert!(service.port_is_unpinned());
        assert!(!service.resolved_env("frontend").contains_key(PORT_ENV_KEY));
    }

    #[test]
    fn an_explicitly_pinned_port_env_entry_counts_as_pinned() {
        let mut service = sample_service();
        service.env.insert(PORT_ENV_KEY.to_string(), "3207".into());

        assert!(!service.port_is_unpinned());
    }

    #[test]
    fn duplicate_ports_are_rejected_and_name_both_services() {
        let err = parse_krab_config(
            r#"
[services.auth]
command = "cargo"
port = 3001

[services.users]
command = "cargo"
port = 3001
"#,
        )
        .expect_err("two services on one port must be rejected");

        let message = err.to_string();
        assert!(message.contains("'auth'"), "unexpected error: {message}");
        assert!(message.contains("'users'"), "unexpected error: {message}");
        assert!(message.contains("3001"), "unexpected error: {message}");
    }

    #[test]
    fn duplicate_resolved_service_names_are_rejected() {
        let err = parse_krab_config(
            r#"
[services.auth]
command = "cargo"
port = 3001
service_name = "shared"

[services.users]
command = "cargo"
port = 3002
service_name = "shared"
"#,
        )
        .expect_err("two services under one identity must be rejected");

        let message = err.to_string();
        assert!(message.contains("'shared'"), "unexpected error: {message}");
        assert!(message.contains("'auth'"), "unexpected error: {message}");
        assert!(message.contains("'users'"), "unexpected error: {message}");
    }

    #[test]
    fn port_zero_is_rejected() {
        let err = parse_krab_config(
            r#"
[services.auth]
command = "cargo"
port = 0
"#,
        )
        .expect_err("port 0 must be rejected");

        assert!(
            err.to_string().contains("port = 0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn services_without_declared_ports_are_still_accepted() {
        // Downstream manifests predate this field. They keep loading; they
        // simply do not get the protection.
        let parsed = parse_krab_config(
            r#"
[services.auth]
command = "cargo"

[services.users]
command = "cargo"
"#,
        )
        .expect("a manifest without ports is still valid");

        assert_eq!(parsed.services.len(), 2);
        assert!(parsed.services["auth"].port_is_unpinned());
    }

    #[test]
    fn probe_url_port_reads_the_forms_that_appear_in_manifests() {
        assert_eq!(probe_url_port("http://127.0.0.1:3001/ready"), Some(3001));
        assert_eq!(probe_url_port("https://localhost:8443/ready"), Some(8443));
        assert_eq!(probe_url_port("http://[::1]:3000/ready"), Some(3000));
        // No explicit port, so nothing to disagree with.
        assert_eq!(probe_url_port("http://localhost/ready"), None);
        assert_eq!(probe_url_port("http://[::1]/ready"), None);
    }

    #[test]
    fn the_workspace_manifest_gives_every_service_a_distinct_port() {
        // Absent when the crate is consumed outside the framework checkout.
        let Some(raw) = workspace_manifest() else {
            return;
        };

        let parsed = parse_krab_config(&raw).expect("workspace krab.toml must be valid");
        let mut ports = Vec::new();
        for (name, service) in &parsed.services {
            let port = service
                .port
                .unwrap_or_else(|| panic!("service '{name}' declares no port"));
            ports.push(port);
        }
        ports.sort_unstable();
        assert_eq!(ports, vec![3000, 3001, 3002, 3207]);
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
