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
            .arg("db-postgres rest")
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
            .arg("db-postgres rest")
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
            .arg("db-postgres rest")
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
            .arg("db-postgres rest")
            .arg("test_migration_rollback")
            .arg("--")
            .arg("--nocapture"),
        diagnostics,
    )?;

    let run_id = std::env::var("GITHUB_RUN_ID").unwrap_or_else(|_| "local".to_string());
    let sha = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local".to_string());
    let git_ref = std::env::var("GITHUB_REF").unwrap_or_else(|_| "local".to_string());
    let timestamp = rfc3339_utc_now();

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
        if check_code_pattern_present(file, "security_headers_middleware") {
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
        if check_code_pattern_present(file, "csrf_protection_middleware") {
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
        if check_code_pattern_present(file, "krab_core::telemetry::init_tracing") {
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
    let timestamp = rfc3339_utc_now();

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

/// The subdirectories of a certification evidence bundle.
///
/// Only sections that actually receive an artifact are modelled here. Earlier
/// revisions also created `02-security`, `05-performance`, `06-observability`
/// and `07-deployment-rehearsal`, none of which was ever written to — an
/// auditor opening the bundle saw four empty directories that read as coverage
/// the certification run does not have. Creating a directory is a claim; make
/// the claim only when there is a file to back it.
struct ReleaseEvidenceBundle {
    test_and_lint: PathBuf,
    compatibility: PathBuf,
    migrations: PathBuf,
    signoff: PathBuf,
}

fn create_release_evidence_bundle(root: &Path) -> Result<ReleaseEvidenceBundle> {
    let bundle = ReleaseEvidenceBundle {
        test_and_lint: root.join("01-test-and-lint"),
        compatibility: root.join("03-contract-and-compatibility"),
        migrations: root.join("04-migrations-and-rollback"),
        signoff: root.join("08-signoff"),
    };

    for path in [
        &bundle.test_and_lint,
        &bundle.compatibility,
        &bundle.migrations,
        &bundle.signoff,
    ] {
        fs::create_dir_all(path)
            .with_context(|| format!("Failed to create evidence directory {}", path.display()))?;
    }

    Ok(bundle)
}

/// Run a subprocess gate and persist its real output as the artifact.
///
/// The output is captured rather than inherited so the bundle preserves the
/// clippy/test/fmt transcript an auditor needs; a failing step is echoed to the
/// console so a local run stays debuggable without opening the bundle.
fn record_command_step(
    name: &'static str,
    artifact: &Path,
    diagnostics: bool,
    cmd: &mut Command,
) -> CertificationStepReport {
    let (result, captured) = run_command_captured(name, cmd, diagnostics);
    record_step_result(name, artifact, result, Some(&captured))
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
    // Honest accounting: these steps are in-process Rust functions that
    // themselves shell out through `run_command_logged`, which inherits this
    // process's stdio. Their child-process output therefore goes straight to
    // the console and is not available to capture here. Passing `None` makes
    // the artifact say so rather than imply a transcript it does not have.
    record_step_result(name, artifact, result, None)
}

fn record_step_result(
    name: &'static str,
    artifact: &Path,
    result: Result<()>,
    captured_output: Option<&str>,
) -> CertificationStepReport {
    let (status, error) = match result {
        Ok(()) => ("passed", None),
        Err(err) => ("failed", Some(err.to_string())),
    };

    let mut body = format!("step: {name}\nstatus: {status}\n");
    if let Some(err) = &error {
        body.push_str(&format!("error: {err}\n"));
    }
    match captured_output {
        Some(output) => {
            body.push_str("\n--- captured output (stdout + stderr) ---\n");
            if output.trim().is_empty() {
                body.push_str("(command produced no output)\n");
            } else {
                body.push_str(output);
                if !output.ends_with('\n') {
                    body.push('\n');
                }
            }
        }
        None => body.push_str(
            "\nnote: this step ran in-process and the cargo invocations it makes \
             inherit this process's stdio, so their output was streamed to the \
             console and could not be captured here. Only the outcome above is \
             recorded for this step.\n",
        ),
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

/// Where `latest.json` / `latest.md` are written, derived from the `--out` root.
///
/// This used to be a hardcoded `internal/audit/release-certify`, so a developer
/// who installed the CLI from crates.io and ran `krab release certify` in their
/// own project got a Krab-specific `internal/audit/` tree created inside it,
/// wherever they had pointed `--out`.
///
/// Deriving the index from the bundle's parent was chosen over "skip the index
/// when there is no `internal/` directory" because it keeps this repository's
/// and CI's behaviour byte-for-byte identical while being correct everywhere
/// else: `--out internal/audit/release-certify/run-<id>` (how
/// `.github/workflows/ops-hardening.yaml` invokes it, and the shape of the
/// default) still indexes at `internal/audit/release-certify/latest.json`, the
/// exact path that workflow uploads. A downstream `--out ./evidence/run-1`
/// indexes at `./evidence/latest.json` — beside the run it describes, and
/// nowhere else. A root with no parent (`--out evidence`) indexes into itself.
fn certification_index_root(out: &Path) -> PathBuf {
    match out.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => out.to_path_buf(),
    }
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
    let index_root = certification_index_root(root);
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

/// Run a command capturing its stdout and stderr, returning both the outcome
/// and the combined transcript.
///
/// Unlike [`run_command_logged`] this does not inherit stdio, because the
/// transcript is the artifact a certification bundle exists to preserve. To
/// keep a local run debuggable the transcript is echoed to the console whenever
/// the command fails (or whenever `--diagnostics` is on); a passing step stays
/// quiet, which is what a multi-gate certification run wants.
fn run_command_captured(label: &str, cmd: &mut Command, diagnostics: bool) -> (Result<()>, String) {
    if diagnostics {
        println!("   > Running: {label}");
    }
    let started = std::time::Instant::now();
    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) => {
            let message = format!("Failed to execute command: {label}: {err}");
            return (Err(anyhow::anyhow!(message.clone())), message);
        }
    };

    let mut transcript = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !transcript.is_empty() && !transcript.ends_with('\n') {
            transcript.push('\n');
        }
        transcript.push_str(&stderr);
    }

    if diagnostics {
        println!(
            "   > Finished: {label} (status: {}, elapsed: {}ms)",
            output.status,
            started.elapsed().as_millis()
        );
    }

    if output.status.success() {
        if diagnostics && !transcript.trim().is_empty() {
            println!("{transcript}");
        }
        (Ok(()), transcript)
    } else {
        if !diagnostics && !transcript.trim().is_empty() {
            println!("{transcript}");
        }
        (Err(anyhow::anyhow!("Command failed: {label}")), transcript)
    }
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

/// Whether a source file counts as a production source for gate purposes.
///
/// A release gate asks "does the shipped code do this?", so a hit inside an
/// integration test, a benchmark, or an example is not evidence. Files under a
/// `tests/`, `benches/` or `examples/` directory, and files named `*_test.rs` /
/// `*_tests.rs` (the in-`src` test modules this workspace uses, e.g.
/// `krab_core/src/db_tests.rs`), are excluded.
fn is_production_source(path: &Path) -> bool {
    let excluded_dir = path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests") | Some("benches") | Some("examples")
        )
    });
    if excluded_dir {
        return false;
    }

    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) => !(stem.ends_with("_test") || stem.ends_with("_tests")),
        None => false,
    }
}

/// Strip `//` line comments and `/* */` block comments from Rust source.
///
/// Deliberately not string-literal aware: a `//` inside a string literal ends
/// the line early, which can only ever drop code from consideration, never add
/// it. For a presence gate that bias is the safe one — it risks a false
/// negative (a loud, investigable failure) instead of a false positive (a gate
/// that silently passes on a mention in prose).
fn strip_rust_comments(source: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;

    for line in source.lines() {
        let mut rest = line;
        loop {
            if in_block {
                match rest.find("*/") {
                    Some(idx) => {
                        in_block = false;
                        rest = &rest[idx + 2..];
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            } else {
                let line_comment = rest.find("//");
                let block_comment = rest.find("/*");
                match (line_comment, block_comment) {
                    (Some(l), Some(b)) if l < b => {
                        out.push_str(&rest[..l]);
                        rest = "";
                        break;
                    }
                    (_, Some(b)) => {
                        out.push_str(&rest[..b]);
                        in_block = true;
                        rest = &rest[b + 2..];
                    }
                    (Some(l), None) => {
                        out.push_str(&rest[..l]);
                        rest = "";
                        break;
                    }
                    (None, None) => break,
                }
            }
        }
        out.push_str(rest);
        out.push('\n');
    }

    out
}

/// Whether `pattern` appears in executable code in a production source file.
///
/// This used to be a bare `content.contains(pattern)` over every `.rs` file,
/// which meant a mention in a doc comment satisfied a release gate — the
/// `csrf_strategy` gate was in fact being answered by prose in
/// `krab_core/src/csrf.rs`, not by the middleware being wired up. Comments and
/// non-production sources are now excluded.
fn check_code_pattern_present(path: &Path, pattern: &str) -> bool {
    if !is_production_source(path) {
        return false;
    }

    match fs::read_to_string(path) {
        Ok(content) => strip_rust_comments(&content).contains(pattern),
        Err(_) => false,
    }
}

/// Days elapsed since 1970-01-01 converted to a proleptic Gregorian date.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the whole range this
/// can produce and needs no calendar tables. Kept in std rather than adding a
/// `chrono`/`time` dependency for one timestamp.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let mp = (5 * day_of_year + 2) / 153; // [0, 11], March-based
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]

    (year + i64::from(month <= 2), month, day)
}

/// Render Unix epoch seconds as an RFC 3339 UTC timestamp.
///
/// The predecessor of this function was named `chrono_like_utc_now` but emitted
/// bare epoch seconds, so `latest.md` rendered `- timestamp: 1786...` while
/// every fixture in this module used ISO-8601 — nothing connected the two.
fn rfc3339_utc_from_unix_secs(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

fn rfc3339_utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    rfc3339_utc_from_unix_secs(now.as_secs())
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
        build_release_certification_index, certification_index_root, check_code_pattern_present,
        collect_secrets_policy_check, create_release_evidence_bundle, finish_release_check,
        is_production_source, record_command_step, record_function_step,
        render_release_certification_index_markdown, rfc3339_utc_from_unix_secs, rfc3339_utc_now,
        strip_rust_comments, CertificationStepReport, EnvVarGuard, ReleaseCertificationReport,
        ReleaseCheckReport,
    };
    use serial_test::serial;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Absolute path to a file in the workspace, from this crate's manifest dir.
    fn workspace_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(relative)
    }

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

    /// `chrono_like_utc_now` returned bare epoch seconds, so `latest.md`
    /// rendered `- timestamp: 1786...` while every fixture in this module used
    /// ISO-8601 — the mismatch was invisible because nothing tested the
    /// formatter. These are the known-good conversions.
    #[test]
    fn rfc3339_formatting_matches_known_epochs() {
        for (secs, expected) in [
            (0_u64, "1970-01-01T00:00:00Z"),
            // Time-of-day carry: one hour, one minute and one second in.
            (3661, "1970-01-01T01:01:01Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (86_400, "1970-01-02T00:00:00Z"),
            // Leap day of a leap century (2000 is divisible by 400).
            (951_782_400, "2000-02-29T00:00:00Z"),
            (951_868_800, "2000-03-01T00:00:00Z"),
            // Year boundary, straddled to the second.
            (1_735_689_599, "2024-12-31T23:59:59Z"),
            (1_735_689_600, "2025-01-01T00:00:00Z"),
            // Leap day of a leap year that is not a century.
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            // 2100 is divisible by 4 but not by 400, so it has no 29 February.
            (4_107_456_000, "2100-02-28T00:00:00Z"),
            (4_107_542_400, "2100-03-01T00:00:00Z"),
        ] {
            assert_eq!(
                rfc3339_utc_from_unix_secs(secs),
                expected,
                "epoch {secs} should render as {expected}"
            );
        }
    }

    /// The regression itself: the timestamp stamped into `summary.json` and
    /// `latest.md` must be a timestamp, not a number.
    #[test]
    fn rfc3339_now_is_not_bare_epoch_seconds() {
        let now = rfc3339_utc_now();
        assert_eq!(now.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ, got {now}");
        assert!(now.ends_with('Z'), "{now} should be UTC-qualified");
        assert!(now.contains('T'), "{now} should separate date and time");
        assert!(
            now.parse::<u64>().is_err(),
            "{now} parsed as an integer, so it is still epoch seconds"
        );
    }

    #[test]
    fn comments_are_stripped_before_pattern_matching() {
        let stripped = strip_rust_comments(
            "/// doc mentions security_headers_middleware\n\
             let x = 1; // trailing mentions csrf_protection_middleware\n\
             /* block mentions init_tracing */ let y = 2;\n\
             let z = 3;\n",
        );

        assert!(!stripped.contains("security_headers_middleware"));
        assert!(!stripped.contains("csrf_protection_middleware"));
        assert!(!stripped.contains("init_tracing"));
        assert!(stripped.contains("let x = 1;"));
        assert!(stripped.contains("let y = 2;"));
        assert!(stripped.contains("let z = 3;"));
    }

    #[test]
    fn multi_line_block_comments_are_stripped() {
        let stripped = strip_rust_comments(
            "let a = 1;\n\
             /*\n\
             security_headers_middleware\n\
             */\n\
             let b = 2;\n",
        );

        assert!(!stripped.contains("security_headers_middleware"));
        assert!(stripped.contains("let a = 1;"));
        assert!(stripped.contains("let b = 2;"));
    }

    /// The gate answered "is CSRF wired up?" with a doc comment. A mention in
    /// prose must not satisfy it; a real reference must.
    #[test]
    fn pattern_gate_ignores_comments_and_accepts_real_usage() {
        let dir = tempfile::tempdir().expect("tempdir");

        let commented = dir.path().join("commented.rs");
        std::fs::write(
            &commented,
            "/// Set by `csrf_protection_middleware`, which lives elsewhere.\n\
             pub const NAME: &str = \"csrf\";\n",
        )
        .expect("write");
        assert!(
            !check_code_pattern_present(&commented, "csrf_protection_middleware"),
            "a doc-comment mention must not satisfy a release gate"
        );

        let real = dir.path().join("real.rs");
        std::fs::write(
            &real,
            "use krab_core::http_security::csrf_protection_middleware;\n\
             let app = router.layer(from_fn(csrf_protection_middleware));\n",
        )
        .expect("write");
        assert!(
            check_code_pattern_present(&real, "csrf_protection_middleware"),
            "a real reference must still satisfy the gate"
        );
    }

    #[test]
    fn pattern_gate_skips_non_production_sources() {
        assert!(is_production_source(Path::new("services/svc/src/main.rs")));
        assert!(is_production_source(Path::new(
            "crates/framework/krab_core/src/http.rs"
        )));
        assert!(!is_production_source(Path::new(
            "crates/framework/krab_core/tests/http_tests.rs"
        )));
        assert!(!is_production_source(Path::new("benches/render.rs")));
        assert!(!is_production_source(Path::new(
            "examples/reference_apps/islands_rpc/src/lib.rs"
        )));
        // In-`src` test modules, the convention this workspace uses
        // (`krab_core/src/db_tests.rs`, `src/api_tests.rs`).
        assert!(!is_production_source(Path::new(
            "crates/framework/krab_core/src/db_tests.rs"
        )));

        let dir = tempfile::tempdir().expect("tempdir");
        let tests_dir = dir.path().join("tests");
        std::fs::create_dir_all(&tests_dir).expect("mkdir");
        let in_tests = tests_dir.join("smoke.rs");
        std::fs::write(&in_tests, "fn t() { security_headers_middleware(); }\n").expect("write");
        assert!(!check_code_pattern_present(
            &in_tests,
            "security_headers_middleware"
        ));
    }

    /// The tightened predicate must still find the three things the
    /// `secure_headers`, `csrf_strategy` and `telemetry_initialization` gates
    /// look for, in the real tree. If a refactor moves them, this fails here
    /// rather than turning `krab release check` red for a mysterious reason.
    #[test]
    fn release_gate_patterns_are_still_found_in_this_repository() {
        let http = workspace_path("crates/framework/krab_core/src/http.rs");
        assert!(http.exists(), "{} should exist", http.display());
        assert!(
            check_code_pattern_present(&http, "security_headers_middleware"),
            "secure_headers gate lost its evidence in http.rs"
        );
        assert!(
            check_code_pattern_present(&http, "csrf_protection_middleware"),
            "csrf_strategy gate lost its evidence in http.rs"
        );

        let auth_main = workspace_path("services/service_auth/src/main.rs");
        assert!(auth_main.exists(), "{} should exist", auth_main.display());
        assert!(
            check_code_pattern_present(&auth_main, "krab_core::telemetry::init_tracing"),
            "telemetry_initialization gate lost its evidence in service_auth"
        );

        // And the false positive that used to answer the csrf gate: csrf.rs
        // mentions the middleware only in a doc comment.
        let csrf = workspace_path("crates/framework/krab_core/src/csrf.rs");
        assert!(csrf.exists(), "{} should exist", csrf.display());
        assert!(
            !check_code_pattern_present(&csrf, "csrf_protection_middleware"),
            "csrf.rs mentions the middleware only in prose; it must not count"
        );
    }

    /// The bundle used to create eight directories and write to four, so
    /// `02-security`, `05-performance`, `06-observability` and
    /// `07-deployment-rehearsal` shipped empty — coverage the run does not have.
    #[test]
    fn evidence_bundle_creates_only_sections_that_receive_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("run-1");
        create_release_evidence_bundle(&root).expect("bundle");

        for present in [
            "01-test-and-lint",
            "03-contract-and-compatibility",
            "04-migrations-and-rollback",
            "08-signoff",
        ] {
            assert!(root.join(present).is_dir(), "{present} should exist");
        }
        for absent in [
            "02-security",
            "05-performance",
            "06-observability",
            "07-deployment-rehearsal",
        ] {
            assert!(
                !root.join(absent).exists(),
                "{absent} is never written to and must not be created"
            );
        }
    }

    /// Every artifact file used to contain three lines restating the summary.
    /// The bundle is consumed by `.github/workflows/release-attestation.yaml`,
    /// so the command transcript is the entire point of it.
    #[test]
    fn command_step_artifact_contains_captured_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("cargo-version.txt");

        let step = record_command_step(
            "cargo-version",
            &artifact,
            false,
            Command::new("cargo").arg("--version"),
        );

        assert_eq!(step.status, "passed");
        let body = std::fs::read_to_string(&artifact).expect("artifact should exist");
        assert!(body.contains("step: cargo-version"));
        assert!(body.contains("status: passed"));
        assert!(body.contains("--- captured output (stdout + stderr) ---"));
        assert!(
            body.contains("cargo "),
            "artifact should hold the real command output, got:\n{body}"
        );
    }

    #[test]
    fn failed_command_step_artifact_captures_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("bad.txt");

        let step = record_command_step(
            "bad-subcommand",
            &artifact,
            false,
            Command::new("cargo").arg("krab-no-such-subcommand-xyz"),
        );

        assert_eq!(step.status, "failed");
        let body = std::fs::read_to_string(&artifact).expect("artifact should exist");
        assert!(body.contains("status: failed"));
        assert!(body.contains("error: Command failed: bad-subcommand"));
        assert!(
            body.contains("krab-no-such-subcommand-xyz"),
            "stderr from the failing command should be preserved, got:\n{body}"
        );
    }

    /// In-process steps shell out with inherited stdio, so their transcript is
    /// genuinely unavailable. The artifact must say so rather than imply the
    /// output was captured.
    #[test]
    fn function_step_artifact_is_explicit_about_uncaptured_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("fn-step.txt");

        let step = record_function_step("in-process", &artifact, || Ok(()));

        assert_eq!(step.status, "passed");
        let body = std::fs::read_to_string(&artifact).expect("artifact should exist");
        assert!(body.contains("status: passed"));
        assert!(body.contains("could not be captured here"));
        assert!(!body.contains("--- captured output"));
    }

    /// The index root was hardcoded to `internal/audit/release-certify`
    /// regardless of `--out`, so running certify in a downstream project
    /// created a Krab-specific `internal/audit/` tree inside it.
    #[test]
    fn index_root_derives_from_the_output_root() {
        // How CI invokes it, and the shape of the default: unchanged behaviour.
        assert_eq!(
            certification_index_root(Path::new("internal/audit/release-certify/run-42")),
            PathBuf::from("internal/audit/release-certify")
        );
        assert_eq!(
            certification_index_root(Path::new("internal/audit/release-certify/local")),
            PathBuf::from("internal/audit/release-certify")
        );
        // A downstream project indexes beside its own bundle, not in an
        // invented `internal/` tree.
        assert_eq!(
            certification_index_root(Path::new("evidence/run-1")),
            PathBuf::from("evidence")
        );
        // A root with no parent indexes into itself.
        assert_eq!(
            certification_index_root(Path::new("evidence")),
            PathBuf::from("evidence")
        );
    }
}
