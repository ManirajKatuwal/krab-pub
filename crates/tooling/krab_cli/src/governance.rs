use anyhow::Result;
use serde::Serialize;
use std::path::Path;

use crate::release_ops::{
    run_contract_checks, run_db_drift_check, run_db_lifecycle_check, run_db_rollback_check,
    run_db_rollback_rehearsal, run_dependency_gate, run_protocol_contract_checks,
    run_release_certify, run_release_check,
};
use crate::{ContractAction, DbAction, ReleaseAction, SecurityAction};

/// The `--json` shape of the thin command-wrapper gates.
///
/// `contract *`, `db *` and `security dependency-gate` are wrappers around
/// `cargo` subprocesses; they have no structured report of their own, so this
/// stays a minimal envelope rather than inventing structure that does not
/// exist behind it.
#[derive(Debug, Serialize)]
struct GateEnvelope<'a> {
    command: &'a str,
    status: &'static str,
    error: Option<String>,
}

/// Print a `{command, status, error}` envelope for a thin gate.
///
/// Two things this deliberately does not do:
///
/// - It does not swallow the gate's own output. These gates shell out with
///   inherited stdio, so the `cargo` transcript reaches stdout no matter what
///   happens here; the envelope is therefore the *final* line of output, not
///   the whole of it. Pretending otherwise would mean capturing subprocess
///   output and changing what a failing gate shows a developer.
/// - It does not alter the outcome. The gate's `Result` is returned unchanged,
///   so the exit status is identical with and without `--json`, which is the
///   same contract `release_ops::finish_release_check` documents.
fn finish_gate(command: &str, json: bool, result: Result<()>) -> Result<()> {
    if !json {
        return result;
    }

    let envelope = match &result {
        Ok(()) => GateEnvelope {
            command,
            status: "passed",
            error: None,
        },
        Err(err) => GateEnvelope {
            command,
            status: "failed",
            error: Some(format!("{err:#}")),
        },
    };

    if let Ok(rendered) = serde_json::to_string_pretty(&envelope) {
        println!("{rendered}");
    }

    result
}

/// The workspace member whose presence identifies a Krab framework checkout.
///
/// `krab_core` is the one crate every framework governance gate compiles, and
/// no downstream project has a reason to declare it as a *member* — a project
/// depends on it, from crates.io or by `path`, which puts it under
/// `[dependencies]`, never under `[workspace] members`. Checking the manifest
/// rather than only the directory also rejects a project that happens to vendor
/// the framework source somewhere under the same relative path.
const FRAMEWORK_MARKER_MEMBER: &str = "crates/framework/krab_core";

/// Whether `root` is the root of a Krab framework checkout.
///
/// Both halves must hold: `root/Cargo.toml` declares
/// [`FRAMEWORK_MARKER_MEMBER`] in `[workspace] members`, and that directory
/// exists. An unreadable or unparseable manifest is "no" — the gates below it
/// could not have run against it either.
pub(crate) fn is_framework_workspace_at(root: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(root.join("Cargo.toml")) else {
        return false;
    };
    let Ok(manifest) = toml::from_str::<toml::Value>(&raw) else {
        return false;
    };
    let declares_core = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .is_some_and(|members| {
            members
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|member| {
                    member.replace('\\', "/").trim_end_matches('/') == FRAMEWORK_MARKER_MEMBER
                })
        });

    declares_core && root.join(FRAMEWORK_MARKER_MEMBER).is_dir()
}

/// The refusal a framework-only gate gives outside a framework checkout.
///
/// These gates run `cargo test -p service_auth`, read
/// `services/service_users/contracts/...`, and scan `services/` and
/// `crates/framework/`. In a `krab new` project none of that exists, and the
/// failure used to surface as a cargo "package ID specification did not match"
/// error or a missing-file error — both reading as a defect in the user's
/// project. One message that says what the command is for, and what to run
/// instead, replaces them.
pub(crate) fn framework_only_refusal(command: &str) -> String {
    format!(
        "`krab {command}` validates the Krab framework's own reference services \
         (service_auth, service_users, krab_core) and only runs inside a Krab framework \
         checkout; this directory is not one (./Cargo.toml has no `{FRAMEWORK_MARKER_MEMBER}` \
         workspace member). For your project, run `cargo test`, `krab doctor --strict`, \
         `krab topology doctor` and `krab security dependency-gate` instead."
    )
}

/// Refuse to run a framework-only gate outside a framework checkout.
fn require_framework_workspace(command: &str) -> Result<()> {
    if is_framework_workspace_at(Path::new(".")) {
        Ok(())
    } else {
        anyhow::bail!("{}", framework_only_refusal(command))
    }
}

/// Run a framework-only gate, or refuse it with the standard message.
///
/// The refusal goes through `finish_gate` like any other failure, so `--json`
/// still prints an envelope and the exit status is the same in both modes.
fn framework_gate(command: &str, json: bool, gate: impl FnOnce() -> Result<()>) -> Result<()> {
    let result = require_framework_workspace(command).and_then(|()| gate());
    finish_gate(command, json, result)
}

pub(super) fn dispatch_contract_action(
    action: &ContractAction,
    diagnostics: bool,
    json: bool,
) -> Result<()> {
    match action {
        ContractAction::Check => {
            framework_gate("contract check", json, || run_contract_checks(diagnostics))
        }
        ContractAction::ProtocolCheck => framework_gate("contract protocol-check", json, || {
            run_protocol_contract_checks(diagnostics)
        }),
    }
}

pub(super) fn dispatch_db_action(action: &DbAction, diagnostics: bool, json: bool) -> Result<()> {
    match action {
        DbAction::Lifecycle => {
            framework_gate("db lifecycle", json, || run_db_lifecycle_check(diagnostics))
        }
        DbAction::Rollback => {
            framework_gate("db rollback", json, || run_db_rollback_check(diagnostics))
        }
        DbAction::Drift => framework_gate("db drift", json, || run_db_drift_check(diagnostics)),
        // The default path is resolved inside the closure, after the
        // workspace check, so a refused run does not also print the
        // artifact-root deprecation warning.
        DbAction::Rehearsal { out } => framework_gate("db rehearsal", json, || {
            let out = out
                .clone()
                .unwrap_or_else(crate::artifacts::default_rehearsal_evidence_path);
            run_db_rollback_rehearsal(&out, diagnostics)
        }),
    }
}

pub(super) fn dispatch_security_action(
    action: &SecurityAction,
    diagnostics: bool,
    json: bool,
) -> Result<()> {
    match action {
        SecurityAction::DependencyGate => finish_gate(
            "security dependency-gate",
            json,
            run_dependency_gate(diagnostics),
        ),
    }
}

pub(super) fn dispatch_release_action(
    action: &ReleaseAction,
    diagnostics: bool,
    json: bool,
) -> Result<()> {
    // Not routed through `finish_gate`: both release commands print their own
    // structured report under `--json`. A refusal is an ordinary error, which
    // exits non-zero in either output mode.
    match action {
        ReleaseAction::Check => {
            require_framework_workspace("release check")?;
            run_release_check(diagnostics, json)
        }
        ReleaseAction::Certify { out } => {
            require_framework_workspace("release certify")?;
            let out = out
                .clone()
                .unwrap_or_else(crate::artifacts::default_release_certify_dir);
            run_release_certify(&out, diagnostics, json)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{finish_gate, framework_only_refusal, is_framework_workspace_at};
    use std::fs;
    use std::path::Path;

    fn workspace_fixture(manifest: &str, create_core_dir: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.toml"), manifest).expect("write manifest");
        if create_core_dir {
            fs::create_dir_all(dir.path().join("crates/framework/krab_core")).expect("mkdir");
        }
        dir
    }

    /// The real checkout this test runs from must be recognised, or every
    /// framework gate would refuse to run in CI.
    #[test]
    fn this_repository_is_recognised_as_the_framework_workspace() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        assert!(is_framework_workspace_at(&repo_root));
    }

    #[test]
    fn a_generated_project_manifest_is_not_the_framework_workspace() {
        // A `krab new` project depends on krab_core; it never lists it as a
        // workspace member, and has no [workspace] at all.
        let dir = workspace_fixture(
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             krab_core = { path = \"../krab/crates/framework/krab_core\" }\n",
            false,
        );
        assert!(!is_framework_workspace_at(dir.path()));
    }

    #[test]
    fn a_directory_without_a_manifest_is_not_the_framework_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!is_framework_workspace_at(dir.path()));
    }

    /// Both halves are required: the member declaration and the directory.
    #[test]
    fn the_member_declaration_alone_or_the_directory_alone_is_not_enough() {
        let declared_only = workspace_fixture(
            "[workspace]\nmembers = [\"crates/framework/krab_core\"]\n",
            false,
        );
        assert!(!is_framework_workspace_at(declared_only.path()));

        let directory_only = workspace_fixture("[workspace]\nmembers = [\"services/app\"]\n", true);
        assert!(!is_framework_workspace_at(directory_only.path()));

        let both = workspace_fixture(
            "[workspace]\nmembers = [\n    \"crates/framework/krab_core\",\n]\n",
            true,
        );
        assert!(is_framework_workspace_at(both.path()));
    }

    #[test]
    fn an_unparseable_manifest_is_not_the_framework_workspace() {
        let dir = workspace_fixture("this is [not toml", true);
        assert!(!is_framework_workspace_at(dir.path()));
    }

    /// One message, naming the command, what it is for, and what to run
    /// instead — the refusal is the whole user-facing surface of this change.
    #[test]
    fn the_refusal_names_the_command_its_purpose_and_the_alternatives() {
        let message = framework_only_refusal("db lifecycle");

        assert!(message.contains("`krab db lifecycle`"), "{message}");
        assert!(
            message.contains("Krab framework's own reference services"),
            "{message}"
        );
        for alternative in [
            "cargo test",
            "krab doctor",
            "krab topology doctor",
            "krab security dependency-gate",
        ] {
            assert!(message.contains(alternative), "{message}");
        }
        assert_eq!(message.lines().count(), 1, "{message}");
    }

    /// The contract `--json` must never break: output mode changes what is
    /// printed, not whether the process exits non-zero. CI reads exit codes.
    #[test]
    fn the_json_envelope_does_not_change_the_exit_outcome() {
        assert!(finish_gate("db drift", true, Ok(())).is_ok());
        assert!(finish_gate("db drift", false, Ok(())).is_ok());
        assert!(finish_gate("db drift", true, Err(anyhow::anyhow!("boom"))).is_err());
        assert!(finish_gate("db drift", false, Err(anyhow::anyhow!("boom"))).is_err());
    }

    /// The error text has to survive into the envelope, or a machine consumer
    /// learns only that something failed.
    #[test]
    fn the_envelope_carries_the_command_name_and_error_text() {
        let envelope = super::GateEnvelope {
            command: "security dependency-gate",
            status: "failed",
            error: Some("Command failed: cargo deny".to_string()),
        };

        let rendered = serde_json::to_string(&envelope).expect("envelope serializes");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("envelope round-trips");

        assert_eq!(parsed["command"], "security dependency-gate");
        assert_eq!(parsed["status"], "failed");
        assert_eq!(parsed["error"], "Command failed: cargo deny");
    }
}
