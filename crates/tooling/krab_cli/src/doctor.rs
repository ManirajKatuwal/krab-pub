use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::project_model::ProjectModel;
use crate::topology::topology_doctor_report;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    fn label(self) -> &'static str {
        match self {
            CheckLevel::Ok => "OK",
            CheckLevel::Warn => "WARN",
            CheckLevel::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorCheck {
    name: &'static str,
    level: CheckLevel,
    details: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Deserialize, Default)]
struct DoctorToml {
    #[serde(default)]
    services: BTreeMap<String, DoctorService>,
}

#[derive(Debug, Deserialize, Default)]
struct DoctorService {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    startup_dependencies: Vec<String>,
    #[serde(default)]
    healthcheck_url: Option<String>,
    #[serde(default)]
    healthcheck: Option<DoctorHealthProbe>,
}

#[derive(Debug, Deserialize, Default)]
struct DoctorHealthProbe {
    #[serde(default)]
    url: Option<String>,
}

pub(crate) fn dispatch_doctor_command(diagnostics: bool, strict: bool) -> Result<()> {
    println!("Running workspace doctor...");
    let report = collect_doctor_report()?;

    let mut warnings = 0usize;
    let mut failures = 0usize;
    for check in &report.checks {
        match check.level {
            CheckLevel::Warn => warnings += 1,
            CheckLevel::Fail => failures += 1,
            CheckLevel::Ok => {}
        }

        println!("[{}] {}", check.level.label(), check.name);
        if diagnostics || check.level != CheckLevel::Ok {
            for detail in &check.details {
                println!("  - {detail}");
            }
        }
    }

    if failures > 0 || (strict && warnings > 0) {
        anyhow::bail!(
            "workspace doctor found {failures} failing check(s) and {warnings} warning check(s)"
        );
    }

    if warnings > 0 {
        println!("Workspace doctor completed with {warnings} warning check(s).");
    } else {
        println!("Workspace doctor passed.");
    }

    Ok(())
}

fn collect_doctor_report() -> Result<DoctorReport> {
    Ok(DoctorReport {
        checks: vec![
            evaluate_project_model()?,
            evaluate_service_configuration()?,
            evaluate_environment_policy(),
            evaluate_topology()?,
        ],
    })
}

fn evaluate_project_model() -> Result<DoctorCheck> {
    let model = ProjectModel::load()?;
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut notes = vec![
        format!("frontend_bin={}", model.frontend_bin),
        format!("bootstrap_bin={}", model.bootstrap_bin),
        format!("dist_dir={}", model.dist_dir.display()),
    ];

    if !Path::new("krab.toml").exists() {
        warnings.push(
            "krab.toml not found; CLI will fall back to workspace-default project model"
                .to_string(),
        );
    }

    if model.frontend_bin.trim().is_empty() {
        failures.push("frontend_bin is empty".to_string());
    }
    if model.bootstrap_bin.trim().is_empty() {
        failures.push("bootstrap_bin is empty".to_string());
    }
    if model.server_paths.is_empty() {
        failures.push("server_paths is empty".to_string());
    }

    for path in &model.server_paths {
        if !path.exists() {
            failures.push(format!(
                "configured server path does not exist: {}",
                path.display()
            ));
        }
    }
    for path in &model.public_paths {
        if !path.exists() {
            warnings.push(format!(
                "configured public path does not exist: {}",
                path.display()
            ));
        }
    }

    if model.has_client_build() {
        match &model.client_crate_dir {
            Some(path) if !path.exists() => failures.push(format!(
                "configured client crate directory does not exist: {}",
                path.display()
            )),
            Some(path) => notes.push(format!("client_crate_dir={}", path.display())),
            None => failures.push("client build configured without client_crate_dir".to_string()),
        }

        for path in &model.client_paths {
            if !path.exists() {
                failures.push(format!(
                    "configured client path does not exist: {}",
                    path.display()
                ));
            }
        }
    }

    let mut details = Vec::new();
    details.extend(notes);
    details.extend(failures.clone());
    details.extend(warnings.clone());

    Ok(DoctorCheck {
        name: "project-model",
        level: choose_level(&failures, &warnings),
        details,
    })
}

fn evaluate_service_configuration() -> Result<DoctorCheck> {
    let config_path = PathBuf::from("krab.toml");
    if !config_path.exists() {
        return Ok(DoctorCheck {
            name: "service-config",
            level: CheckLevel::Warn,
            details: vec!["krab.toml missing; service configuration checks skipped".to_string()],
        });
    }

    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let parsed: DoctorToml = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let notes = vec![format!("services={}", parsed.services.len())];

    if parsed.services.is_empty() {
        warnings.push("no [services.*] entries found in krab.toml".to_string());
    }

    for (name, service) in &parsed.services {
        if service
            .command
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        {
            failures.push(format!("service `{name}` has no command configured"));
        }

        if let Some(cwd) = &service.cwd {
            let path = PathBuf::from(cwd);
            if !path.exists() {
                failures.push(format!(
                    "service `{name}` cwd does not exist: {}",
                    path.display()
                ));
            }
        }

        for dependency in service
            .depends_on
            .iter()
            .chain(service.startup_dependencies.iter())
        {
            if !parsed.services.contains_key(dependency) {
                failures.push(format!(
                    "service `{name}` references unknown dependency `{dependency}`"
                ));
            }
        }

        let ready_probe = service
            .healthcheck
            .as_ref()
            .and_then(|probe| probe.url.as_deref())
            .or(service.healthcheck_url.as_deref());

        match ready_probe {
            Some(url) => {
                if !url.contains("/ready") {
                    warnings.push(format!(
                        "service `{name}` readiness probe should target `/ready`, found `{url}`"
                    ));
                }
            }
            None => failures.push(format!(
                "service `{name}` has no readiness probe configured in krab.toml"
            )),
        }
    }

    let mut details = Vec::new();
    details.extend(notes);
    details.extend(failures.clone());
    details.extend(warnings.clone());

    Ok(DoctorCheck {
        name: "service-config",
        level: choose_level(&failures, &warnings),
        details,
    })
}

fn evaluate_environment_policy() -> DoctorCheck {
    let warnings = collect_environment_warnings();
    let mut details = Vec::new();
    if warnings.is_empty() {
        details.push("environment policy checks produced no warnings".to_string());
    } else {
        details.extend(warnings.clone());
    }

    DoctorCheck {
        name: "environment-policy",
        level: if warnings.is_empty() {
            CheckLevel::Ok
        } else {
            CheckLevel::Warn
        },
        details,
    }
}

fn evaluate_topology() -> Result<DoctorCheck> {
    let report = topology_doctor_report()?;
    let mut details = vec![
        format!("checked_rust_files={}", report.checked_rust_files),
        format!("contract_path={}", report.contract_path.display()),
    ];
    details.extend(report.violations.clone());

    Ok(DoctorCheck {
        name: "topology-boundaries",
        level: if report.violations.is_empty() {
            CheckLevel::Ok
        } else {
            CheckLevel::Fail
        },
        details,
    })
}

fn choose_level(failures: &[String], warnings: &[String]) -> CheckLevel {
    if !failures.is_empty() {
        CheckLevel::Fail
    } else if !warnings.is_empty() {
        CheckLevel::Warn
    } else {
        CheckLevel::Ok
    }
}

fn collect_environment_warnings() -> Vec<String> {
    let mut warnings = Vec::new();

    let auth_mode = std::env::var("KRAB_AUTH_MODE").unwrap_or_else(|_| "jwt".to_string());
    if auth_mode.eq_ignore_ascii_case("jwt") || auth_mode.eq_ignore_ascii_case("oidc") {
        if std::env::var("KRAB_OIDC_ISSUER").is_err() {
            warnings.push("KRAB_OIDC_ISSUER is required when KRAB_AUTH_MODE=jwt".to_string());
        }
        if std::env::var("KRAB_OIDC_AUDIENCE").is_err() {
            warnings.push("KRAB_OIDC_AUDIENCE is required when KRAB_AUTH_MODE=jwt".to_string());
        }
    } else if auth_mode.eq_ignore_ascii_case("static") {
        let env_name = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
        if !env_name.eq_ignore_ascii_case("local") && !env_name.eq_ignore_ascii_case("dev") {
            warnings.push(
                "KRAB_AUTH_MODE=static is forbidden outside local/dev; use jwt or oidc".to_string(),
            );
        }
    } else {
        warnings.push(format!(
            "Unsupported KRAB_AUTH_MODE='{auth_mode}'; expected static|jwt|oidc"
        ));
    }

    let env_name = std::env::var("KRAB_ENVIRONMENT").unwrap_or_else(|_| "dev".to_string());
    if !["local", "dev", "staging", "prod"].contains(&env_name.as_str()) {
        warnings.push(format!(
            "KRAB_ENVIRONMENT should be one of local|dev|staging|prod, found: {env_name}"
        ));
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::{choose_level, CheckLevel};

    #[test]
    fn choose_level_prefers_failures() {
        let level = choose_level(&["missing".to_string()], &["warn".to_string()]);
        assert_eq!(level, CheckLevel::Fail);
    }

    #[test]
    fn choose_level_uses_warn_when_only_warnings_exist() {
        let level = choose_level(&[], &["warn".to_string()]);
        assert_eq!(level, CheckLevel::Warn);
    }

    #[test]
    fn choose_level_is_ok_when_empty() {
        let level = choose_level(&[], &[]);
        assert_eq!(level, CheckLevel::Ok);
    }
}
