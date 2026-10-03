//! Where the orchestrator writes per-service log artifacts.
//!
//! The logs used to go to a hard-coded `internal/audit/orchestrator/`, a
//! directory that only exists in the Krab framework maintainer's untracked
//! working copy. In any other project the orchestrator created a Krab-internal
//! tree beside the user's code, outside every `.gitignore` they had.
//!
//! The rule is the one `krab_cli` applies to its own `--out` defaults
//! (`crates/tooling/krab_cli/src/artifacts.rs`); this crate does not depend on
//! the CLI, so it is restated here — keep the two in step:
//!
//! 1. `KRAB_ARTIFACT_DIR`, when set to a non-empty value;
//! 2. `internal/audit/`, when that directory exists — deprecated, logged as a
//!    warning, removed in 0.7.0;
//! 3. `.krab/`, which `krab new` ignores in the generated `.gitignore`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tracing::warn;

pub(crate) const ARTIFACT_DIR_ENV: &str = "KRAB_ARTIFACT_DIR";
pub(crate) const DEFAULT_ARTIFACT_ROOT: &str = ".krab";
pub(crate) const LEGACY_ARTIFACT_ROOT: &str = "internal/audit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactRootSource {
    Env,
    LegacyInternalAudit,
    Default,
}

/// Resolve the artifact root for `cwd` and an explicit `KRAB_ARTIFACT_DIR`.
///
/// Pure in both inputs so the precedence is testable without mutating the
/// process environment or CWD. An empty override counts as unset — an
/// unfilled template line is far likelier than a request to write logs into
/// the working directory itself.
pub(crate) fn resolve_artifact_root_in(
    cwd: &Path,
    env_value: Option<OsString>,
) -> (PathBuf, ArtifactRootSource) {
    if let Some(value) = env_value.filter(|value| !value.is_empty()) {
        return (PathBuf::from(value), ArtifactRootSource::Env);
    }
    if cwd.join(LEGACY_ARTIFACT_ROOT).is_dir() {
        return (
            PathBuf::from(LEGACY_ARTIFACT_ROOT),
            ArtifactRootSource::LegacyInternalAudit,
        );
    }
    (
        PathBuf::from(DEFAULT_ARTIFACT_ROOT),
        ArtifactRootSource::Default,
    )
}

/// The directory service logs go under, for this process.
pub(crate) fn orchestrator_artifact_root() -> PathBuf {
    let (root, source) =
        resolve_artifact_root_in(Path::new("."), std::env::var_os(ARTIFACT_DIR_ENV));
    if source == ArtifactRootSource::LegacyInternalAudit {
        warn!(
            artifact_root = LEGACY_ARTIFACT_ROOT,
            removed_in = "0.7.0",
            override_env = ARTIFACT_DIR_ENV,
            "orchestrator_artifact_root_legacy_fallback_deprecated"
        );
    }
    root.join("orchestrator")
}

#[cfg(test)]
mod tests {
    use super::{resolve_artifact_root_in, ArtifactRootSource};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn a_plain_directory_defaults_to_dot_krab() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_artifact_root_in(dir.path(), None),
            (PathBuf::from(".krab"), ArtifactRootSource::Default)
        );
    }

    #[test]
    fn an_existing_internal_audit_is_a_deprecated_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("internal/audit")).expect("mkdir");
        assert_eq!(
            resolve_artifact_root_in(dir.path(), None),
            (
                PathBuf::from("internal/audit"),
                ArtifactRootSource::LegacyInternalAudit
            )
        );
    }

    #[test]
    fn the_override_wins_and_an_empty_one_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("internal/audit")).expect("mkdir");
        assert_eq!(
            resolve_artifact_root_in(dir.path(), Some(OsString::from("logs"))),
            (PathBuf::from("logs"), ArtifactRootSource::Env)
        );

        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_artifact_root_in(plain.path(), Some(OsString::new())).1,
            ArtifactRootSource::Default
        );
    }
}
