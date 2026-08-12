use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::project_model::ProjectModel;
use crate::topology::{
    topology_doctor_report, CHECK_CONTRACT_PAYLOAD_DERIVES, CHECK_SERVICE_SOURCE_SCAN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
enum CheckLevel {
    Ok,
    /// The check did not run because it does not apply to this project.
    ///
    /// Distinct from `Ok` on purpose: rendering "did not run" as green tells
    /// the reader they have coverage they do not have.
    Skip,
    Warn,
    Fail,
}

impl CheckLevel {
    fn label(self) -> &'static str {
        match self {
            CheckLevel::Ok => "OK",
            CheckLevel::Skip => "SKIP",
            CheckLevel::Warn => "WARN",
            CheckLevel::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DoctorCheck {
    name: &'static str,
    level: CheckLevel,
    details: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

/// The `--json` shape of `krab doctor`.
///
/// `success` is the exit-status verdict, computed under the same `--strict`
/// rule the human path uses, so a CI consumer never has to reimplement it.
/// The counts are carried explicitly because `skipped` is deliberately not
/// fatal — a reader tallying levels themselves could easily get that wrong.
#[derive(Debug, Serialize)]
struct DoctorJsonReport<'a> {
    success: bool,
    failures: usize,
    warnings: usize,
    skipped: usize,
    checks: &'a [DoctorCheck],
}

#[derive(Debug, Deserialize, Default)]
struct DoctorToml {
    #[serde(default)]
    services: BTreeMap<String, DoctorService>,
    /// Only its presence is read. A `krab.toml` carrying `[project]` but no
    /// `[services.*]` is the shape `krab new` scaffolds — a single-service
    /// project with nothing for the orchestrator to run — which is why the
    /// absence of services there is reported as not-applicable rather than as
    /// a warning. See [`evaluate_service_configuration`].
    #[serde(default)]
    project: Option<toml::Value>,
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

pub(crate) fn dispatch_doctor_command(diagnostics: bool, strict: bool, json: bool) -> Result<()> {
    if !json {
        println!("Running workspace doctor...");
    }
    let report = collect_doctor_report();

    let mut warnings = 0usize;
    let mut failures = 0usize;
    let mut skips = 0usize;
    for check in &report.checks {
        match check.level {
            CheckLevel::Warn => warnings += 1,
            CheckLevel::Fail => failures += 1,
            CheckLevel::Skip => skips += 1,
            CheckLevel::Ok => {}
        }
    }

    let success = doctor_success(failures, warnings, strict);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DoctorJsonReport {
                success,
                failures,
                warnings,
                skipped: skips,
                checks: &report.checks,
            })?
        );
    } else {
        for check in &report.checks {
            println!("[{}] {}", check.level.label(), check.name);
            if diagnostics || check.level != CheckLevel::Ok {
                for detail in &check.details {
                    println!("  - {detail}");
                }
            }
        }
    }

    // `--json` changes only what is printed, never the exit status.
    if !success {
        anyhow::bail!(
            "workspace doctor found {failures} failing check(s) and {warnings} warning check(s)"
        );
    }

    if json {
        return Ok(());
    }

    // Skips are reported, never fatal — not even under `--strict`. They mean
    // "not applicable here", which is a fact about the project, not a defect
    // in it. They are still counted out loud so the summary line cannot be
    // mistaken for full coverage.
    let skipped_note = if skips > 0 {
        format!(" ({skips} check(s) skipped as not applicable)")
    } else {
        String::new()
    };

    if warnings > 0 {
        println!("Workspace doctor completed with {warnings} warning check(s){skipped_note}.");
    } else {
        println!("Workspace doctor passed{skipped_note}.");
    }

    Ok(())
}

/// The exit-status rule for `krab doctor`, in one place.
///
/// `--json` reports this as `success` and the process exits on the same value,
/// so a consumer reading the field and a consumer reading the exit code cannot
/// disagree. Skips are absent on purpose: a skipped check means "not applicable
/// to this project", never a defect in it, so it must not sway the verdict —
/// not even under `--strict`.
fn doctor_success(failures: usize, warnings: usize, strict: bool) -> bool {
    !(failures > 0 || (strict && warnings > 0))
}

/// Collect every check, unconditionally.
///
/// This used to be `Ok(DoctorReport { checks: vec![evaluate_project_model()?,
/// ...] })`. The `?` on each evaluator meant a single `Err` threw away the
/// checks that had already succeeded and `krab doctor` printed nothing but the
/// error — in a generated project, `evaluate_topology` failed on a missing
/// framework file and the passing project-model, service-config and
/// environment-policy results never reached the terminal. An evaluator that
/// cannot run is now itself a `FAIL` entry carrying the error text, so the
/// report is always complete.
fn collect_doctor_report() -> DoctorReport {
    DoctorReport {
        checks: vec![
            check_or_failure("project-model", evaluate_project_model()),
            check_or_failure("service-config", evaluate_service_configuration()),
            evaluate_environment_policy(),
            check_or_failure("topology-boundaries", evaluate_topology()),
        ],
    }
}

/// Turn an evaluator's `Err` into a failing check instead of aborting the run.
///
/// `Fail` rather than `Warn`: an evaluator that errored produced no verdict at
/// all, so the safe reading is "unproven", and `krab doctor` must exit non-zero
/// on it exactly as it did before this became recoverable.
fn check_or_failure(name: &'static str, result: Result<DoctorCheck>) -> DoctorCheck {
    match result {
        Ok(check) => check,
        Err(err) => DoctorCheck {
            name,
            level: CheckLevel::Fail,
            details: vec![format!("check could not run: {err:#}")],
        },
    }
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
    evaluate_service_configuration_at(&PathBuf::from("krab.toml"))
}

/// Path-parameterised so tests can point at a fixture instead of mutating the
/// process CWD, which races with the rest of the suite. Mirrors
/// `topology::topology_doctor_report_at`.
fn evaluate_service_configuration_at(config_path: &Path) -> Result<DoctorCheck> {
    if !config_path.exists() {
        // `Skip`, not `Warn`: the detail line always said "skipped", and
        // reporting a check that never ran as a warning made `--strict` fail
        // for a project that simply does not use an orchestrator manifest.
        return Ok(DoctorCheck {
            name: "service-config",
            level: CheckLevel::Skip,
            details: vec!["krab.toml missing; service configuration checks skipped".to_string()],
        });
    }

    let raw = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let parsed: DoctorToml = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let notes = vec![format!("services={}", parsed.services.len())];

    if parsed.services.is_empty() {
        // A scaffold from `krab new` has `[project]` and no `[services.*]`:
        // one binary, nothing for the orchestrator to supervise. Warning about
        // that made `krab doctor --strict` — a command the generated README
        // and CI both invoke — fail on every freshly generated project, which
        // is the framework workspace's multi-service shape leaking into
        // projects that never asked for it. Absent `[project]` the file is not
        // a recognisable project at all, so the warning still stands there.
        if parsed.project.is_some() {
            return Ok(DoctorCheck {
                name: "service-config",
                level: CheckLevel::Skip,
                details: vec![
                    "skipped: krab.toml declares [project] and no [services.*]; orchestrator \
                     service checks do not apply to a single-service project"
                        .to_string(),
                ],
            });
        }
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
    let warnings = crate::env_policy::collect_environment_warnings();
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
    let mut details = Vec::new();
    if report.ran(CHECK_SERVICE_SOURCE_SCAN) {
        details.push(format!("checked_rust_files={}", report.checked_rust_files));
    }
    if report.ran(CHECK_CONTRACT_PAYLOAD_DERIVES) {
        details.push(format!("contract_path={}", report.contract_path.display()));
    }
    for entry in &report.skipped {
        details.push(format!("skipped {}: {}", entry.check, entry.reason));
    }
    details.extend(report.violations.clone());

    // A real violation still fails. Otherwise, if any sub-check was skipped the
    // whole check reports SKIP rather than OK — outside a framework checkout
    // most of what this check covers does not exist, and an OK there would be
    // claiming boundary coverage that was never computed.
    let level = if !report.violations.is_empty() {
        CheckLevel::Fail
    } else if !report.skipped.is_empty() {
        CheckLevel::Skip
    } else {
        CheckLevel::Ok
    };

    Ok(DoctorCheck {
        name: "topology-boundaries",
        level,
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

#[cfg(test)]
mod tests {
    use super::{
        check_or_failure, choose_level, collect_doctor_report, doctor_success,
        evaluate_service_configuration_at, CheckLevel, DoctorCheck, DoctorJsonReport,
    };

    fn krab_toml_fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("krab.toml");
        std::fs::write(&path, body).expect("write fixture");
        (dir, path)
    }

    /// `krab new` scaffolds a `krab.toml` with `[project]` and no `[services.*]`
    /// — one binary, nothing for the orchestrator to supervise. Reporting that
    /// as a warning made `krab doctor --strict` exit non-zero on every freshly
    /// generated project, because `--strict` promotes warnings to failures. The
    /// generated README and the scaffolded CI both run that command, so the
    /// framework workspace's multi-service shape was failing projects that had
    /// done nothing wrong.
    #[test]
    fn a_single_service_scaffold_does_not_warn_about_absent_orchestrator_services() {
        let (_dir, path) = krab_toml_fixture(
            r#"
[project]
frontend_bin = "demo_app"
server_paths = ["src"]
"#,
        );

        let check = evaluate_service_configuration_at(&path).expect("check should not error");

        // Assert on the level, not on the wording: the skip's own explanation
        // mentions `[services.*]`, so a substring check for that phrase matches
        // the passing case too. The level is what `--strict` acts on.
        assert_eq!(check.level, CheckLevel::Skip);
        assert!(
            check.details.iter().all(|d| d.starts_with("skipped:")),
            "a skipped check should report only why it was skipped: {:?}",
            check.details
        );
    }

    /// The counterpart: the skip is keyed on `[project]`, not on "services is
    /// empty". A manifest with neither section is not a recognisable project,
    /// and must still be flagged rather than quietly waved through.
    #[test]
    fn a_manifest_with_neither_section_still_warns() {
        let (_dir, path) = krab_toml_fixture("[something_else]\nkey = \"value\"\n");

        let check = evaluate_service_configuration_at(&path).expect("check should not error");

        assert_eq!(check.level, CheckLevel::Warn);
        assert!(
            check.details.iter().any(|d| d.contains("no [services.*]")),
            "{:?}",
            check.details
        );
    }

    /// And a manifest that *does* declare services is still validated in full —
    /// the skip must not become a blanket escape from the readiness-probe and
    /// restart-policy rules.
    #[test]
    fn declared_services_are_still_validated() {
        let (_dir, path) = krab_toml_fixture(
            r#"
[project]
frontend_bin = "demo_app"

[services.frontend]
cwd = "does/not/exist"
"#,
        );

        let check = evaluate_service_configuration_at(&path).expect("check should not error");

        assert_eq!(check.level, CheckLevel::Fail);
        assert!(
            check.details.iter().any(|d| d.contains("no command")),
            "{:?}",
            check.details
        );
    }

    /// A skipped check must never be printed with the same marker as a passing
    /// one — that is the whole point of the level existing.
    #[test]
    fn skip_is_labelled_distinctly_from_ok() {
        assert_eq!(CheckLevel::Skip.label(), "SKIP");
        assert_ne!(CheckLevel::Skip.label(), CheckLevel::Ok.label());
    }

    /// The evaluator's error text has to survive into the report, or the user
    /// gets a bare FAIL with nothing to act on.
    #[test]
    fn evaluator_error_becomes_a_failing_check_carrying_the_error_text() {
        let check = check_or_failure(
            "topology-boundaries",
            Err(anyhow::anyhow!("Failed reading service_contract.rs")),
        );

        assert_eq!(check.name, "topology-boundaries");
        assert_eq!(check.level, CheckLevel::Fail);
        assert!(
            check.details[0].contains("Failed reading service_contract.rs"),
            "{:?}",
            check.details
        );
    }

    /// Regression guard for the bug this restructuring exists to fix: one
    /// evaluator returning `Err` used to discard every check that had already
    /// succeeded, so `krab doctor` printed the error and nothing else. The
    /// report must always carry all four checks.
    ///
    /// Runs against the crate directory (the CWD `cargo test` uses), which is
    /// not a framework workspace root — exactly the shape that used to abort.
    #[test]
    #[serial_test::serial]
    fn doctor_report_keeps_every_check_even_outside_a_framework_workspace() {
        let report = collect_doctor_report();

        let names: Vec<&str> = report.checks.iter().map(|check| check.name).collect();
        assert_eq!(
            names,
            vec![
                "project-model",
                "service-config",
                "environment-policy",
                "topology-boundaries",
            ]
        );

        let topology = report
            .checks
            .iter()
            .find(|check| check.name == "topology-boundaries")
            .expect("topology check present");
        assert_ne!(
            topology.level,
            CheckLevel::Fail,
            "missing framework paths must skip, not fail: {:?}",
            topology.details
        );
    }

    /// `--json` exists so CI can read the verdict without scraping `[WARN]`
    /// prefixes out of the human output. The level strings are therefore part
    /// of the CLI's surface, not an implementation detail.
    #[test]
    fn the_json_report_carries_every_check_with_a_stable_level_string() {
        let checks = vec![
            DoctorCheck {
                name: "project-model",
                level: CheckLevel::Ok,
                details: vec!["frontend_bin=demo".to_string()],
            },
            DoctorCheck {
                name: "service-config",
                level: CheckLevel::Skip,
                details: vec!["krab.toml missing".to_string()],
            },
            DoctorCheck {
                name: "environment-policy",
                level: CheckLevel::Warn,
                details: vec!["KRAB_OIDC_ISSUER is required".to_string()],
            },
            DoctorCheck {
                name: "topology-boundaries",
                level: CheckLevel::Fail,
                details: vec!["boom".to_string()],
            },
        ];

        let rendered = serde_json::to_string(&DoctorJsonReport {
            success: false,
            failures: 1,
            warnings: 1,
            skipped: 1,
            checks: &checks,
        })
        .expect("doctor report serializes");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("doctor report round-trips");

        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["failures"], 1);
        assert_eq!(parsed["warnings"], 1);
        assert_eq!(parsed["skipped"], 1);

        let entries = parsed["checks"].as_array().expect("checks is an array");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0]["name"], "project-model");
        assert_eq!(entries[0]["level"], "ok");
        assert_eq!(entries[1]["level"], "skip");
        assert_eq!(entries[2]["level"], "warn");
        assert_eq!(entries[3]["level"], "fail");
        assert_eq!(entries[2]["details"][0], "KRAB_OIDC_ISSUER is required");
    }

    /// `success` in the JSON is the *same* value that decides the exit status,
    /// computed once. A consumer reading `success` and a consumer reading the
    /// exit code must never disagree, and skips must not sway either.
    #[test]
    fn doctor_success_follows_the_strict_rule_and_ignores_skips() {
        assert!(doctor_success(0, 0, true));
        assert!(doctor_success(0, 1, false), "warnings alone exit zero");
        assert!(!doctor_success(0, 1, true), "--strict promotes warnings");
        assert!(
            !doctor_success(1, 0, false),
            "a failure always exits non-zero"
        );
        assert!(!doctor_success(1, 0, true));
    }

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
