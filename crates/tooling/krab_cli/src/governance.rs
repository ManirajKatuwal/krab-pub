use anyhow::Result;

use crate::release_ops::{
    run_contract_checks, run_db_drift_check, run_db_lifecycle_check, run_db_rollback_check,
    run_db_rollback_rehearsal, run_dependency_gate, run_protocol_contract_checks,
    run_release_certify, run_release_check,
};
use crate::{ContractAction, DbAction, ReleaseAction, SecurityAction};

pub(super) fn dispatch_contract_action(action: &ContractAction) -> Result<()> {
    match action {
        ContractAction::Check { diagnostics } => run_contract_checks(*diagnostics),
        ContractAction::ProtocolCheck { diagnostics } => run_protocol_contract_checks(*diagnostics),
    }
}

pub(super) fn dispatch_db_action(action: &DbAction) -> Result<()> {
    match action {
        DbAction::Lifecycle { diagnostics } => run_db_lifecycle_check(*diagnostics),
        DbAction::Rollback { diagnostics } => run_db_rollback_check(*diagnostics),
        DbAction::Drift { diagnostics } => run_db_drift_check(*diagnostics),
        DbAction::Rehearsal { out, diagnostics } => run_db_rollback_rehearsal(out, *diagnostics),
    }
}

pub(super) fn dispatch_security_action(action: &SecurityAction) -> Result<()> {
    match action {
        SecurityAction::DependencyGate { diagnostics } => run_dependency_gate(*diagnostics),
    }
}

pub(super) fn dispatch_release_action(action: &ReleaseAction) -> Result<()> {
    match action {
        ReleaseAction::Check { diagnostics, json } => run_release_check(*diagnostics, *json),
        ReleaseAction::Certify {
            out,
            diagnostics,
            json,
        } => run_release_certify(out, *diagnostics, *json),
    }
}
