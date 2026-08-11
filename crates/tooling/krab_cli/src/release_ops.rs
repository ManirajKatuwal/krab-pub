use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{
    fs,
    time::{Duration, SystemTime},
};

use crate::topology::collect_rust_files_under;

#[derive(Debug, Serialize)]
struct ReleaseCheckReport {
    success: bool,
    checks: BTreeMap<&'static str, serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct CertificationStepReport {
    name: &'static str,
    artifact: String,
    status: &'static str,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReleaseCertificationReport {
    success: bool,
    evidence_root: String,
    timestamp: String,
    steps: Vec<CertificationStepReport>,
}

#[derive(Debug, Serialize)]
struct ReleaseCertificationIndex {
    success: bool,
    timestamp: String,
    evidence_root: String,
    summary_json: String,
    summary_markdown: String,
    ci_run_id: Option<String>,
    ci_sha: Option<String>,
    ci_ref: Option<String>,
}

pub(super) fn run_contract_checks(diagnostics: bool) -> Result<()> {
    println!("📜 Running API contract checks...");

    run_command_logged(
        "krab_core API envelope contract tests",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("rest")
            .arg("api_tests"),
        diagnostics,
    )?;

    run_command_logged(
        "service_auth contract tests",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("service_auth")
            .arg("contract_"),
        diagnostics,
    )?;

    run_command_logged(
        "service_users contract tests",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("service_users")
            .arg("contract_"),
        diagnostics,
    )?;

    println!("✅ API contract checks passed");
    Ok(())
}

pub(super) fn run_protocol_contract_checks(diagnostics: bool) -> Result<()> {
    println!("🧪 Running protocol contract checks...");

    run_command_logged(
        "service_users parity tests",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("service_users")
            .arg("parity_"),
        diagnostics,
    )?;

    run_command_logged(
        "krab_core protocol tests",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("rest")
            .arg("protocol"),
        diagnostics,
    )?;

    run_split_topology_gateway_conflict_check()?;
    run_protocol_version_compatibility_check()?;
    Ok(())
}

pub(super) fn run_db_lifecycle_check(diagnostics: bool) -> Result<()> {
    run_command_logged(
        "db migration lifecycle checks",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("db rest")
            .arg("test_migration_lifecycle"),
        diagnostics,
    )
}

pub(super) fn run_db_rollback_check(diagnostics: bool) -> Result<()> {
    run_command_logged(
        "db rollback checks",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("db rest")
            .arg("test_migration_rollback"),
        diagnostics,
    )
}

pub(super) fn run_db_drift_check(diagnostics: bool) -> Result<()> {
    run_command_logged(
        "db drift checks",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("db rest")
            .arg("test_drift_detection"),
        diagnostics,
    )
}

pub(super) fn run_db_rollback_rehearsal(out: &PathBuf, diagnostics: bool) -> Result<()> {
    run_command_logged(
        "db rollback rehearsal test",
        Command::new("cargo")
            .arg("test")
            .arg("--package")
            .arg("krab_core")
            .arg("--features")
            .arg("db rest")
            .arg("test_migration_rollback")
            .arg("--")
            .arg("--nocapture"),
        diagnostics,
    )?;

    let run_id = std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "local".to_string());
    let sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local".to_string());
    let git_ref = std::env::var("GITHUB_REF").unwrap_or_else(|_| "local".to_string());
    let timestamp = chrono_like_utc_now();

    let mut evidence = String::new();
    evidence.push_str("rollback_rehearsal: ok\n");
    evidence.push_str(&format!("run_id: {}\n", run_id));
    evidence.push_str(&format!("sha: {}\n", sha));
    evidence.push_str(&format!("ref: {}\n", git_ref));
    evidence.push_str(&format!("timestamp: {}\n", timestamp));

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create rollback evidence directory {parent:?}")
            })?;
        }
    }

    fs::write(out, evidence)
        .with_context(|| format!("Failed to write rollback evidence to {}", out.display()))?;

    println!("✅ Wrote rollback rehearsal evidence to {}", out.display());
    Ok(())
}

pub(super) fn run_dependency_gate(diagnostics: bool) -> Result<()> {
    println!("🔐 Running dependency governance gate...");
    ensure_cargo_subcommand_available("deny", "cargo-deny")?;

    run_command_logged(
        "cargo deny --all-features check advisories licenses bans sources",
        Command::new("cargo")
            .arg("deny")
            .arg("--all-features")
            .arg("check")
            .arg("advisories")
            .arg("licenses")
            .arg("bans")
            .arg("sources"),
        diagnostics,
    )?;

    println!("✅ Dependency governance gate passed");
    Ok(())
}

/// Sets an environment variable for the current scope and restores the prior
/// value (or removes the variable) on drop, so release checks that evaluate
/// policy under `KRAB_ENVIRONMENT=prod` do not leak that setting into later
/// gates running in the same process.
struct EnvVarGuard {
    name: &'static str,
    prior: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prior = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

pub(super) fn run_release_check(diagnostics: bool, json: bool) -> Result<()> {
    if !json {
        println!("🚀 Running pre-flight release checklist...");
    }

    let report = collect_release_check_report(diagnostics);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for (name, result) in &report.checks {
            let status = result
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let icon = if status == "passed" { "✅" } else { "❌" };
            println!("{} {}: {}", icon, name, status);
            if let Some(issues) = result.get("issues") {
                if let Some(arr) = issues.as_array() {
                    for issue in arr {
                        if let Some(s) = issue.as_str() {
                            println!("   - {}", s);
                        }
                    }
                }
            }
        }
        if report.success {
            println!("🎉 All release checks passed!");
        }
    }

    finish_release_check(&report)
}

/// The exit-code contract for `krab release check`: a failed report is a
/// non-zero exit in every output mode. `--json` only changes what is printed,
/// never the exit status.
fn finish_release_check(report: &ReleaseCheckReport) -> Result<()> {
    if report.success {
        Ok(())
    } else {
        anyhow::bail!("One or more release checks failed.");
    }
}

/// Evaluate the secrets policy under prod rules and record the result.
/// Invalid configuration (for example a malformed `KRAB_PORT`) is a failed
/// check, not a panic. Returns whether the check passed.
fn collect_secrets_policy_check(checks: &mut BTreeMap<&'static str, serde_json::Value>) -> bool {
    match krab_core::config::KrabConfig::from_env_checked("krab_cli", 8080) {
        Ok(config) => {
            let secrets_report = config.validate_secrets_sources();
            if secrets_report.has_errors() {
                checks.insert(
                    "secrets_policy",
                    serde_json::json!({
                        "status": "failed",
                        "issues": secrets_report.issues.iter()
                            .filter(|i| i.severity == krab_core::config::SecretIssueSeverity::Error)
                            .map(|i| i.reason.clone())
                            .collect::<Vec<_>>()
                    }),
                );
                false
            } else {
                checks.insert("secrets_policy", serde_json::json!({ "status": "passed" }));
                true
            }
        }
        Err(err) => {
            checks.insert(
                "secrets_policy",
                serde_json::json!({
                    "status": "failed",
                    "issues": [format!("invalid configuration: {err}")]
                }),
            );
            false
        }
    }
}

fn collect_release_check_report(diagnostics: bool) -> ReleaseCheckReport {
    let mut checks = BTreeMap::new();
    let mut all_passed = true;

    // Secrets policy is evaluated under prod rules; the guard restores the
    // caller's KRAB_ENVIRONMENT once the report has been collected.
    let _env_guard = EnvVarGuard::set("KRAB_ENVIRONMENT", "prod");
    if !collect_secrets_policy_check(&mut checks) {
        all_passed = false;
    }

    let mut headers_present = false;
    let mut files = Vec::new();
    let _ = collect_rust_files_under(Path::new("services"), &mut files);
    let _ = collect_rust_files_under(Path::new("crates/framework"), &mut files);
    for file in &files {
        if check_code_pattern_present(file.to_str().unwrap_or(""), "security_headers_middleware") {
            headers_present = true;
            break;
        }
    }
    checks.insert(
        "secure_headers",
        serde_json::json!({ "status": if headers_present { "passed" } else { "failed" } }),
    );
    if !headers_present {
        all_passed = false;
    }

    let mut csrf_present = false;
    for file in &files {
        if check_code_pattern_present(file.to_str().unwrap_or(""), "csrf_protection_middleware") {
            csrf_present = true;
            break;
        }
    }
    checks.insert(
        "csrf_strategy",
        serde_json::json!({ "status": if csrf_present { "passed" } else { "failed" } }),
    );
    if !csrf_present {
        all_passed = false;
    }

    let mut telemetry_present = false;
    for file in &files {
        if check_code_pattern_present(
            file.to_str().unwrap_or(""),
            "krab_core::telemetry::init_tracing",
        ) {
            telemetry_present = true;
            break;
        }
    }
    checks.insert(
        "telemetry_initialization",
        serde_json::json!({ "status": if telemetry_present { "passed" } else { "failed" } }),
    );
    if !telemetry_present {
        all_passed = false;
    }

    match run_dependency_gate(diagnostics) {
        Ok(()) => {
            checks.insert("dependency_gate", serde_json::json!({ "status": "passed" }));
        }
        Err(err) => {
            checks.insert(
                "dependency_gate",
                serde_json::json!({
                    "status": "failed",
                    "error": err.to_string(),
                }),
            );
            all_passed = false;
        }
    }

    ReleaseCheckReport {
        success: all_passed,
        checks,
    }
}

pub(super) fn run_release_certify(out: &Path, diagnostics: bool, json: bool) -> Result<()> {
    let bundle = create_release_evidence_bundle(out)?;
    let timestamp = chrono_like_utc_now();

    let release_check = collect_release_check_report(diagnostics);
    write_json_artifact(&bundle.signoff.join("release-check.json"), &release_check)?;

    let mut steps = vec![CertificationStepReport {
        name: "release-check",
        artifact: bundle
            .signoff
            .join("release-check.json")
            .display()
            .to_string(),
        status: if release_check.success {
            "passed"
        } else {
            "failed"
        },
        error: None,
    }];

    steps.push(record_command_step(
        "fmt-check",
        &bundle.test_and_lint.join("fmt-check.txt"),
        diagnostics,
        Command::new("cargo").arg("fmt").arg("--all").arg("--check"),
    ));
    steps.push(record_command_step(
        "clippy",
        &bundle.test_and_lint.join("clippy.txt"),
        diagnostics,
        Command::new("cargo")
            .arg("clippy")
            .arg("--workspace")
            .arg("--all-targets")
            .arg("--")
            .arg("-D")
            .arg("warnings"),
    ));
    steps.push(record_command_step(
        "workspace-tests",
        &bundle.test_and_lint.join("workspace-tests.txt"),
        diagnostics,
        Command::new("cargo").arg("test").arg("--workspace"),
    ));

    steps.push(record_function_step(
        "contract-checks",
        &bundle.compatibility.join("contract-checks.txt"),
        || run_contract_checks(diagnostics),
    ));
    steps.push(record_function_step(
        "protocol-contract-checks",
        &bundle.compatibility.join("protocol-contract-checks.txt"),
        || run_protocol_contract_checks(diagnostics),
    ));
    steps.push(record_function_step(
        "db-lifecycle",
        &bundle.migrations.join("db-lifecycle.txt"),
        || run_db_lifecycle_check(diagnostics),
    ));
    steps.push(record_function_step(
        "db-drift",
        &bundle.migrations.join("db-drift.txt"),
        || run_db_drift_check(diagnostics),
    ));
    steps.push(record_function_step(
        "db-rollback-rehearsal",
        &bundle.migrations.join("rollback-rehearsal.txt"),
        || {
            run_db_rollback_rehearsal(
                &bundle.migrations.join("rollback-rehearsal-evidence.txt"),
                diagnostics,
            )
        },
    ));

    let success = release_check.success && steps.iter().all(|step| step.status == "passed");
    let summary = ReleaseCertificationReport {
        success,
        evidence_root: out.display().to_string(),
        timestamp,
        steps,
    };

    write_json_artifact(&bundle.signoff.join("summary.json"), &summary)?;
    fs::write(
        bundle.signoff.join("summary.md"),
        render_certification_summary_markdown(&summary),
    )
    .with_context(|| {
        format!(
            "Failed to write {}",
            bundle.signoff.join("summary.md").display()
        )
    })?;
    write_latest_certification_index(out, &summary)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "Release certification evidence written to {}",
            out.display()
        );
    }

    if !success {
        anyhow::bail!("release certification failed; inspect the evidence bundle for details");
    }

    Ok(())
}

struct ReleaseEvidenceBundle {
    test_and_lint: PathBuf,
    security: PathBuf,
    compatibility: PathBuf,
    migrations: PathBuf,
    performance: PathBuf,
    observability: PathBuf,
    deployment: PathBuf,
    signoff: PathBuf,
}

fn create_release_evidence_bundle(root: &Path) -> Result<ReleaseEvidenceBundle> {
    let bundle = ReleaseEvidenceBundle {
        test_and_lint: root.join("01-test-and-lint"),
        security: root.join("02-security"),
        compatibility: root.join("03-contract-and-compatibility"),
        migrations: root.join("04-migrations-and-rollback"),
        performance: root.join("05-performance"),
        observability: root.join("06-observability"),
        deployment: root.join("07-deployment-rehearsal"),
        signoff: root.join("08-signoff"),
    };

    for path in [
        &bundle.test_and_lint,
        &bundle.security,
        &bundle.compatibility,
        &bundle.migrations,
        &bundle.performance,
        &bundle.observability,
        &bundle.deployment,
        &bundle.signoff,
    ] {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create evidence directory {}", path.display()))?;
    }

    Ok(bundle)
}

fn record_command_step(
    name: &'static str,
    artifact: &Path,
    diagnostics: bool,
    cmd: &mut Command,
) -> CertificationStepReport {
    let result = run_command_logged(name, cmd, diagnostics);
    record_step_result(name, artifact, result)
}

fn record_function_step<F>(
    name: &'static str,
    artifact: &Path,
    action: F,
) -> CertificationStepReport
where
    F: FnOnce() -> Result<()>,
{
    let result = action();
    record_step_result(name, artifact, result)
}

fn record_step_result(
    name: &'static str,
    artifact: &Path,
    result: Result<()>,
) -> CertificationStepReport {
    let (status, error) = match result {
        Ok(()) => ("passed", None),
        Err(err) => ("failed", Some(err.to_string())),
    };

    let mut body = format!("step: {name}\nstatus: {status}\n");
    if let Some(err) = &error {
        body.push_str(&format!("error: {err}\n"));
    }
    let _ = fs::write(artifact, body);

    CertificationStepReport {
        name,
        artifact: artifact.display().to_string(),
        status,
        error,
    }
}

fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn render_certification_summary_markdown(summary: &ReleaseCertificationReport) -> String {
    let mut out = String::new();
    out.push_str("# Release Certification Summary\n\n");
    out.push_str(&format!("- success: {}\n", summary.success));
    out.push_str(&format!("- timestamp: {}\n", summary.timestamp));
    out.push_str(&format!("- evidence_root: {}\n\n", summary.evidence_root));
    out.push_str("| Step | Status | Artifact |\n| --- | --- | --- |\n");
    for step in &summary.steps {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            step.name, step.status, step.artifact
        ));
        if let Some(error) = &step.error {
            out.push_str(&format!("error: {}\n", error));
        }
    }
    out
}

fn certification_index_root() -> PathBuf {
    PathBuf::from("internal/audit/release-certify")
}

fn path_for_docs(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn build_release_certification_index(
    root: &Path,
    summary: &ReleaseCertificationReport,
) -> ReleaseCertificationIndex {
    ReleaseCertificationIndex {
        success: summary.success,
        timestamp: summary.timestamp.clone(),
        evidence_root: path_for_docs(root),
        summary_json: path_for_docs(&root.join("08-signoff/summary.json")),
        summary_markdown: path_for_docs(&root.join("08-signoff/summary.md")),
        ci_run_id: std::env::var("GITHUB_RUN_ID").ok(),
        ci_sha: std::env::var("GITHUB_SHA").ok(),
        ci_ref: std::env::var("GITHUB_REF").ok(),
    }
}

fn write_latest_certification_index(
    root: &Path,
    summary: &ReleaseCertificationReport,
) -> Result<()> {
    let index_root = certification_index_root();
    fs::create_dir_all(&index_root)
        .with_context(|| format!("Failed to create {}", index_root.display()))?;
    let index = build_release_certification_index(root, summary);
    write_json_artifact(&index_root.join("latest.json"), &index)?;
    fs::write(
        index_root.join("latest.md"),
        render_release_certification_index_markdown(&index),
    )
    .with_context(|| format!("Failed to write {}", index_root.join("latest.md").display()))?;
    Ok(())
}

fn render_release_certification_index_markdown(index: &ReleaseCertificationIndex) -> String {
    let mut out = String::new();
    out.push_str("# Latest Release Certification Evidence\n\n");
    out.push_str(&format!("- success: {}\n", index.success));
    out.push_str(&format!("- timestamp: {}\n", index.timestamp));
    out.push_str(&format!("- evidence_root: {}\n", index.evidence_root));
    out.push_str(&format!("- summary_json: {}\n", index.summary_json));
    out.push_str(&format!("- summary_markdown: {}\n", index.summary_markdown));
    if let Some(run_id) = &index.ci_run_id {
        out.push_str(&format!("- ci_run_id: {}\n", run_id));
    }
    if let Some(sha) = &index.ci_sha {
        out.push_str(&format!("- ci_sha: {}\n", sha));
    }
    if let Some(git_ref) = &index.ci_ref {
        out.push_str(&format!("- ci_ref: {}\n", git_ref));
    }
    out
}

pub(super) fn run_command_logged(label: &str, cmd: &mut Command, diagnostics: bool) -> Result<()> {
    if diagnostics {
        println!("   > Running: {label}");
    }
    let started = std::time::Instant::now();
    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute command: {label}"))?;
    if diagnostics {
        println!(
            "   > Finished: {label} (status: {}, elapsed: {}ms)",
            status,
            started.elapsed().as_millis()
        );
    }
    if !status.success() {
        anyhow::bail!("Command failed: {label}");
    }
    Ok(())
}

fn ensure_cargo_subcommand_available(subcommand: &str, install_package: &str) -> Result<()> {
    let status = Command::new("cargo")
        .arg(subcommand)
        .arg("--version")
        .status()
        .with_context(|| format!("failed to execute `cargo {subcommand} --version`"))?;

    if status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "Required cargo subcommand is missing: `cargo {}`. Install with: `cargo install {}`",
        subcommand,
        install_package
    );
}

fn check_code_pattern_present(path: &str, pattern: &str) -> bool {
    if let Ok(content) = fs::read_to_string(path) {
        content.contains(pattern)
    } else {
        false
    }
}

fn chrono_like_utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    format!("{}", now.as_secs())
}

fn run_split_topology_gateway_conflict_check() -> Result<()> {
    let mapping_path =
        PathBuf::from("services/service_users/contracts/users_gateway_upstreams_v1.json");
    let raw = fs::read_to_string(&mapping_path)
        .with_context(|| format!("failed reading {}", mapping_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid json in {}", mapping_path.display()))?;

    let routes = parsed
        .get("routes")
        .or_else(|| parsed.get("upstreams"))
        .and_then(|v| v.as_array())
        .context("gateway contract missing 'routes' (or legacy 'upstreams') array")?;

    let mut seen = std::collections::BTreeSet::new();
    for route in routes {
        let upstream = route
            .get("upstream")
            .or_else(|| route.get("service"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let path_prefix = route
            .get("path_prefix")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let path_exact = route
            .get("path_exact")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let key = format!("upstream={upstream}|prefix={path_prefix}|exact={path_exact}");
        if !seen.insert(key.clone()) {
            anyhow::bail!("duplicate upstream mapping detected: {key}");
        }
    }
    Ok(())
}

fn run_protocol_version_compatibility_check() -> Result<()> {
    for bin in [
        "services/service_users/src/bin/users_rest.rs",
        "services/service_users/src/bin/users_graphql.rs",
        "services/service_users/src/bin/users_rpc.rs",
    ] {
        if !PathBuf::from(bin).exists() {
            anyhow::bail!("missing expected protocol split binary source: {bin}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_release_certification_index, collect_secrets_policy_check, finish_release_check,
        render_release_certification_index_markdown, CertificationStepReport, EnvVarGuard,
        ReleaseCertificationReport, ReleaseCheckReport,
    };
    use serial_test::serial;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[test]
    fn failed_release_check_report_exits_nonzero_regardless_of_output_mode() {
        let mut checks = BTreeMap::new();
        checks.insert("secrets_policy", serde_json::json!({ "status": "failed" }));
        let failed = ReleaseCheckReport {
            success: false,
            checks,
        };

        // `finish_release_check` is the single exit path for both `--json` and
        // plain output; a failed report must be an error (non-zero exit).
        assert!(finish_release_check(&failed).is_err());

        let passed = ReleaseCheckReport {
            success: true,
            checks: BTreeMap::new(),
        };
        assert!(finish_release_check(&passed).is_ok());
    }

    #[test]
    fn failed_release_check_report_serializes_success_false() {
        let mut checks = BTreeMap::new();
        checks.insert("secrets_policy", serde_json::json!({ "status": "failed" }));
        let report = ReleaseCheckReport {
            success: false,
            checks,
        };

        let rendered = serde_json::to_string_pretty(&report).expect("report should serialize");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("report should round-trip");
        assert_eq!(parsed.get("success"), Some(&serde_json::Value::Bool(false)));
    }

    #[test]
    #[serial]
    fn env_var_guard_restores_prior_value_and_absence() {
        std::env::set_var("KRAB_RELEASE_OPS_TEST_VAR", "before");
        {
            let _guard = EnvVarGuard::set("KRAB_RELEASE_OPS_TEST_VAR", "prod");
            assert_eq!(
                std::env::var("KRAB_RELEASE_OPS_TEST_VAR").as_deref(),
                Ok("prod")
            );
        }
        assert_eq!(
            std::env::var("KRAB_RELEASE_OPS_TEST_VAR").as_deref(),
            Ok("before")
        );

        std::env::remove_var("KRAB_RELEASE_OPS_TEST_VAR");
        {
            let _guard = EnvVarGuard::set("KRAB_RELEASE_OPS_TEST_VAR", "prod");
        }
        assert!(std::env::var("KRAB_RELEASE_OPS_TEST_VAR").is_err());
    }

    #[test]
    #[serial]
    fn invalid_krab_port_is_a_failed_check_not_a_panic() {
        let _env = EnvVarGuard::set("KRAB_ENVIRONMENT", "prod");
        let _port = EnvVarGuard::set("KRAB_PORT", "not-a-port");

        let mut checks = BTreeMap::new();
        let passed = collect_secrets_policy_check(&mut checks);

        assert!(!passed, "invalid KRAB_PORT must fail the secrets check");
        let entry = checks
            .get("secrets_policy")
            .expect("secrets_policy check should be recorded");
        assert_eq!(entry.get("status").and_then(|v| v.as_str()), Some("failed"));
        let issues = entry
            .get("issues")
            .and_then(|v| v.as_array())
            .expect("failed check should carry issues");
        assert!(issues
            .iter()
            .any(|issue| issue.as_str().unwrap_or_default().contains("KRAB_PORT")));
    }

    #[test]
    fn certification_index_tracks_summary_paths() {
        let summary = ReleaseCertificationReport {
            success: true,
            evidence_root: "internal/audit/release-certify/run-42".to_string(),
            timestamp: "2026-06-05T00:00:00Z".to_string(),
            steps: vec![CertificationStepReport {
                name: "workspace-tests",
                artifact:
                    "internal/audit/release-certify/run-42/01-test-and-lint/workspace-tests.txt"
                        .to_string(),
                status: "passed",
                error: None,
            }],
        };

        let index = build_release_certification_index(
            Path::new("internal/audit/release-certify/run-42"),
            &summary,
        );
        assert_eq!(index.evidence_root, "internal/audit/release-certify/run-42");
        assert!(index.summary_json.ends_with("08-signoff/summary.json"));
        assert!(index.summary_markdown.ends_with("08-signoff/summary.md"));
    }

    #[test]
    fn certification_index_markdown_links_summary_artifacts() {
        let markdown =
            render_release_certification_index_markdown(&build_release_certification_index(
                Path::new("internal/audit/release-certify/run-77"),
                &ReleaseCertificationReport {
                    success: false,
                    evidence_root: "internal/audit/release-certify/run-77".to_string(),
                    timestamp: "2026-06-05T01:02:03Z".to_string(),
                    steps: vec![],
                },
            ));

        assert!(markdown.contains("Latest Release Certification Evidence"));
        assert!(markdown.contains("internal/audit/release-certify/run-77/08-signoff/summary.json"));
        assert!(markdown.contains("internal/audit/release-certify/run-77/08-signoff/summary.md"));
    }
}
