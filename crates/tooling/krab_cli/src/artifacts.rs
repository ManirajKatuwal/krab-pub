//! Where the CLI writes generated artifacts by default.
//!
//! `krab db rehearsal` and `krab release certify` used to default to paths
//! under `internal/audit/`, a directory that exists only in the Krab framework
//! maintainer's untracked working copy. In any other project the CLI created
//! an `internal/audit/` tree nobody asked for, next to the user's own code and
//! outside every `.gitignore` they had.
//!
//! The default root is now `.krab/`, which `krab new` ignores in the generated
//! `.gitignore`. `KRAB_ARTIFACT_DIR` overrides it. For one release a checkout
//! that already has an `internal/audit/` directory keeps writing there, with a
//! warning, so a maintainer's existing evidence layout does not move without
//! notice; that fallback is removed in 0.7.0.
//!
//! `krab_orchestrator` resolves its log directory by the same rule. It does not
//! depend on this crate, so the rule is restated there rather than shared —
//! keep the two in step.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable that pins the artifact root.
pub(crate) const ARTIFACT_DIR_ENV: &str = "KRAB_ARTIFACT_DIR";

/// The root used when nothing else applies.
pub(crate) const DEFAULT_ARTIFACT_ROOT: &str = ".krab";

/// The pre-0.6.0 root, honoured with a warning while it exists on disk.
pub(crate) const LEGACY_ARTIFACT_ROOT: &str = "internal/audit";

/// Which rule chose the artifact root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactRootSource {
    /// `KRAB_ARTIFACT_DIR` was set to a non-empty value.
    Env,
    /// `internal/audit/` exists in the working directory. Deprecated.
    LegacyInternalAudit,
    /// Neither of the above: `.krab/`.
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactRoot {
    pub(crate) path: PathBuf,
    pub(crate) source: ArtifactRootSource,
}

/// Resolve the artifact root for a working directory and an explicit
/// `KRAB_ARTIFACT_DIR` value.
///
/// Pure in both inputs so the precedence can be tested without touching the
/// process environment or the process CWD. The returned path is relative when
/// the rule that chose it is relative, so output paths print the way a user
/// would type them.
///
/// An empty `KRAB_ARTIFACT_DIR` counts as unset: `KRAB_ARTIFACT_DIR=` in a
/// shell or `.env` file is far more often an unfilled template than a request
/// to write into the working directory itself.
pub(crate) fn resolve_artifact_root_in(cwd: &Path, env_value: Option<OsString>) -> ArtifactRoot {
    if let Some(value) = env_value.filter(|value| !value.is_empty()) {
        return ArtifactRoot {
            path: PathBuf::from(value),
            source: ArtifactRootSource::Env,
        };
    }

    if cwd.join(LEGACY_ARTIFACT_ROOT).is_dir() {
        return ArtifactRoot {
            path: PathBuf::from(LEGACY_ARTIFACT_ROOT),
            source: ArtifactRootSource::LegacyInternalAudit,
        };
    }

    ArtifactRoot {
        path: PathBuf::from(DEFAULT_ARTIFACT_ROOT),
        source: ArtifactRootSource::Default,
    }
}

/// The one-line deprecation notice for the `internal/audit/` fallback.
pub(crate) fn legacy_fallback_warning() -> String {
    format!(
        "warning: writing artifacts under `{LEGACY_ARTIFACT_ROOT}/` because that directory exists; \
         this fallback is removed in 0.7.0 (the default becomes `{DEFAULT_ARTIFACT_ROOT}/`) — set \
         {ARTIFACT_DIR_ENV}={LEGACY_ARTIFACT_ROOT} to keep the current location"
    )
}

/// Resolve the artifact root for the current process, warning on stderr when
/// the deprecated fallback chose it.
///
/// stderr rather than stdout because `release certify --json` prints its
/// report on stdout, and a warning there would make it unparseable.
pub(crate) fn resolve_artifact_root() -> ArtifactRoot {
    let root = resolve_artifact_root_in(Path::new("."), std::env::var_os(ARTIFACT_DIR_ENV));
    if root.source == ArtifactRootSource::LegacyInternalAudit {
        eprintln!("{}", legacy_fallback_warning());
    }
    root
}

/// `krab db rehearsal`'s evidence file when `--out` is not given.
pub(crate) fn default_rehearsal_evidence_path() -> PathBuf {
    resolve_artifact_root()
        .path
        .join("evidence")
        .join("rollback-rehearsal-evidence.txt")
}

/// `krab release certify`'s bundle directory when `--out` is not given.
pub(crate) fn default_release_certify_dir() -> PathBuf {
    resolve_artifact_root()
        .path
        .join("release-certify")
        .join("local")
}

#[cfg(test)]
mod tests {
    use super::{
        legacy_fallback_warning, resolve_artifact_root_in, ArtifactRootSource, ARTIFACT_DIR_ENV,
        DEFAULT_ARTIFACT_ROOT, LEGACY_ARTIFACT_ROOT,
    };
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    /// A user project: no `internal/`, no override. Nothing Krab-internal may
    /// appear in it.
    #[test]
    fn a_plain_directory_defaults_to_dot_krab() {
        let dir = tempfile::tempdir().expect("tempdir");

        let root = resolve_artifact_root_in(dir.path(), None);

        assert_eq!(root.source, ArtifactRootSource::Default);
        assert_eq!(root.path, PathBuf::from(DEFAULT_ARTIFACT_ROOT));
    }

    /// A maintainer checkout keeps its layout for one release, flagged.
    #[test]
    fn an_existing_internal_audit_directory_is_used_as_a_deprecated_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("internal/audit")).expect("mkdir");

        let root = resolve_artifact_root_in(dir.path(), None);

        assert_eq!(root.source, ArtifactRootSource::LegacyInternalAudit);
        assert_eq!(root.path, PathBuf::from(LEGACY_ARTIFACT_ROOT));
    }

    /// `internal/` alone is not enough — only the directory the old defaults
    /// actually wrote into triggers the fallback.
    #[test]
    fn internal_without_audit_does_not_trigger_the_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("internal/plans")).expect("mkdir");

        let root = resolve_artifact_root_in(dir.path(), None);

        assert_eq!(root.source, ArtifactRootSource::Default);
    }

    /// The override beats both the fallback and the default.
    #[test]
    fn the_environment_override_wins_over_the_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("internal/audit")).expect("mkdir");

        let root = resolve_artifact_root_in(dir.path(), Some(OsString::from("build/evidence")));

        assert_eq!(root.source, ArtifactRootSource::Env);
        assert_eq!(root.path, PathBuf::from("build/evidence"));
    }

    #[test]
    fn an_empty_override_counts_as_unset() {
        let dir = tempfile::tempdir().expect("tempdir");

        let root = resolve_artifact_root_in(dir.path(), Some(OsString::new()));

        assert_eq!(root.source, ArtifactRootSource::Default);
    }

    /// The warning is the only notice a maintainer gets before 0.7.0 moves
    /// their evidence, so it has to name the removal and the way to opt out.
    #[test]
    fn the_fallback_warning_names_the_removal_release_and_the_override() {
        let warning = legacy_fallback_warning();

        assert!(warning.contains("0.7.0"), "{warning}");
        assert!(warning.contains(ARTIFACT_DIR_ENV), "{warning}");
        assert!(warning.contains(LEGACY_ARTIFACT_ROOT), "{warning}");
        assert_eq!(warning.lines().count(), 1, "{warning}");
    }

    /// The seam the pure resolver cannot cover: the process reader must feed
    /// `KRAB_ARTIFACT_DIR` into it, and both `--out` defaults must hang off
    /// the resolved root.
    #[test]
    #[serial_test::serial]
    fn the_out_defaults_derive_from_the_process_override() {
        let previous = std::env::var_os(ARTIFACT_DIR_ENV);
        std::env::set_var(ARTIFACT_DIR_ENV, "custom-artifacts");

        let rehearsal = super::default_rehearsal_evidence_path();
        let certify = super::default_release_certify_dir();

        match previous {
            Some(value) => std::env::set_var(ARTIFACT_DIR_ENV, value),
            None => std::env::remove_var(ARTIFACT_DIR_ENV),
        }

        assert_eq!(
            rehearsal,
            PathBuf::from("custom-artifacts/evidence/rollback-rehearsal-evidence.txt")
        );
        assert_eq!(
            certify,
            PathBuf::from("custom-artifacts/release-certify/local")
        );
    }
}
