use anyhow::Result;
use serde::Serialize;

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

pub(super) fn dispatch_contract_action(
    action: &ContractAction,
    diagnostics: bool,
    json: bool,
) -> Result<()> {
    match action {
        ContractAction::Check => {
            finish_gate("contract check", json, run_contract_checks(diagnostics))
        }
        ContractAction::ProtocolCheck => finish_gate(
            "contract protocol-check",
            json,
            run_protocol_contract_checks(diagnostics),
        ),
    }
}

pub(super) fn dispatch_db_action(action: &DbAction, diagnostics: bool, json: bool) -> Result<()> {
    match action {
        DbAction::Lifecycle => {
            finish_gate("db lifecycle", json, run_db_lifecycle_check(diagnostics))
        }
        DbAction::Rollback => finish_gate("db rollback", json, run_db_rollback_check(diagnostics)),
        DbAction::Drift => finish_gate("db drift", json, run_db_drift_check(diagnostics)),
        DbAction::Rehearsal { out } => finish_gate(
            "db rehearsal",
            json,
            run_db_rollback_rehearsal(out, diagnostics),
        ),
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
    match action {
        ReleaseAction::Check => run_release_check(diagnostics, json),
        ReleaseAction::Certify { out } => run_release_certify(out, diagnostics, json),
    }
}

#[cfg(test)]
mod tests {
    use super::finish_gate;

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
