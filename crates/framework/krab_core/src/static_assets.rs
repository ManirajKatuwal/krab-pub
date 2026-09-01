//! Serving static assets safely.
//!
//! Ported from `krab_server` when that crate was removed (see
//! [ADR 0005](https://github.com/ManirajKatuwal/krab-pub/blob/main/docs/adr/0005-krab-server-disposition.md)).
//! `tower_http::services::ServeDir` covers most of what that crate did, but the
//! explicit canonicalise-and-compare in [`resolve_static_pkg_path`] is a
//! security control worth keeping rather than re-deriving at each call site.

use mime_guess::MimeGuess;
use std::path::{Component, Path, PathBuf};

/// Resolve a requested path inside `static_root`, or `None` if it escapes.
///
/// Rejects, in order: absolute paths, any path containing a `..`, a root, or a
/// Windows prefix component, and — after canonicalising both sides — anything
/// that does not actually live under the root. The final check is what catches
/// symlinks, which a purely lexical check cannot.
///
/// `None` means "do not serve this". It is deliberately not an error type: a
/// caller should return 404 rather than reporting why a path was refused, which
/// would confirm the existence of files outside the root.
///
/// # This blocks the calling thread
///
/// Resolving symlinks means real syscalls, and there is no non-blocking way to
/// ask the filesystem what a path actually points at. An `async` handler that
/// calls this directly does that work on an executor worker, so under load a
/// burst of asset requests stalls unrelated requests scheduled on the same
/// worker. Call it from `tokio::task::spawn_blocking`, or serve the directory
/// with `tower_http::services::ServeDir`, which reads through `tokio::fs` and
/// is already off the executor. What is *not* an acceptable fix is dropping the
/// canonicalisation for a lexical prefix check — see below, the syscalls are
/// the control.
///
/// ```
/// use krab_core::static_assets::resolve_static_pkg_path;
///
/// let root = std::env::temp_dir();
/// assert!(resolve_static_pkg_path(&root, "../etc/passwd").is_none());
/// assert!(resolve_static_pkg_path(&root, "/absolute").is_none());
/// ```
pub fn resolve_static_pkg_path(
    static_root: &Path,
    requested_relative_path: &str,
) -> Option<PathBuf> {
    let requested = Path::new(requested_relative_path);
    if requested.is_absolute() {
        return None;
    }

    if requested.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    // Both resolutions happen on every call, deliberately, and neither is
    // cached. Caching the root looks free — a configured static root does not
    // move — but the usual atomic deploy points that root at `releases/<id>`
    // through a symlink and repoints it on the next release. A memoised
    // canonical root would go on serving out of the retired release, including
    // files that release was pulled to remove, and would 404 every asset once
    // the directory is reaped. Re-resolving means the guard compares against
    // what the filesystem says now, not what it said at boot.
    let canonical_root = std::fs::canonicalize(static_root).ok()?;
    let candidate = canonical_root.join(requested);
    // The check the lexical guard above cannot make. `requested` is known to
    // hold no `..` by this point, but any component of it can still be a
    // symlink pointing anywhere on the disk, and a symlink is what an attacker
    // plants once the obvious `../` is refused. Resolving the candidate for
    // real and comparing is the only thing that sees it.
    let canonical_candidate = std::fs::canonicalize(candidate).ok()?;

    if canonical_candidate.starts_with(&canonical_root) {
        Some(canonical_candidate)
    } else {
        None
    }
}

/// Content type for a static asset path.
///
/// Falls back to `application/octet-stream` for anything unrecognised, so an
/// unexpected extension is downloaded rather than interpreted by the browser.
pub fn static_mime_for(path: &str) -> String {
    MimeGuess::from_path(path)
        .first_raw()
        .map(normalize_static_mime)
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// Attach `charset=utf-8` to HTML.
///
/// Without it a browser sniffs the encoding, which is both a rendering bug and
/// an XSS vector for UTF-7-style confusion on older engines.
pub fn normalize_static_mime(mime: &str) -> String {
    if mime == "text/html" {
        "text/html; charset=utf-8".to_string()
    } else {
        mime.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ported unchanged from `krab_server`, which is the point: the control had
    /// tests, and deleting the crate must not delete its coverage.
    #[test]
    fn resolve_static_pkg_path_rejects_parent_dir_traversal() {
        let root = std::env::temp_dir().join("krab_core_static_root_reject");
        std::fs::create_dir_all(&root).expect("failed to create static root");

        assert!(resolve_static_pkg_path(&root, "../secret.txt").is_none());
    }

    #[test]
    fn resolve_static_pkg_path_accepts_file_inside_root() {
        let root = std::env::temp_dir().join("krab_core_static_root_accept");
        std::fs::create_dir_all(&root).expect("failed to create static root");
        let file = root.join("app.js");
        std::fs::write(&file, "console.log('ok');").expect("failed to create static file");

        let resolved = resolve_static_pkg_path(&root, "app.js").expect("expected in-root file");
        let canonical_root = std::fs::canonicalize(&root).expect("failed to canonicalize root");
        assert!(resolved.starts_with(&canonical_root));
    }

    #[test]
    fn resolve_static_pkg_path_rejects_absolute_paths() {
        let root = std::env::temp_dir().join("krab_core_static_root_absolute");
        std::fs::create_dir_all(&root).expect("failed to create static root");

        // Both spellings, so the check holds on Windows as well as Unix.
        assert!(resolve_static_pkg_path(&root, "/etc/passwd").is_none());
        assert!(resolve_static_pkg_path(&root, r"C:\Windows\system.ini").is_none());
    }

    #[test]
    fn resolve_static_pkg_path_rejects_traversal_nested_mid_path() {
        let root = std::env::temp_dir().join("krab_core_static_root_nested");
        std::fs::create_dir_all(&root).expect("failed to create static root");

        // A `..` that does not lead the path is still a `..`.
        assert!(resolve_static_pkg_path(&root, "assets/../../secret.txt").is_none());
    }

    /// Symlink creation is privileged on Windows unless Developer Mode is on,
    /// so this reports failure rather than panicking and the caller skips.
    #[cfg(unix)]
    fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    /// The guard's whole reason for touching the filesystem. Every other
    /// rejection test here is satisfied by lexical inspection alone, so a
    /// refactor that swapped canonicalisation for path normalisation would
    /// leave this module green while reopening the hole: `escape.txt` is one
    /// ordinary component with no `..` in it, and it resolves outside the root.
    #[cfg(any(unix, windows))]
    #[test]
    fn resolve_static_pkg_path_rejects_symlink_escaping_root() {
        let base = std::env::temp_dir().join("krab_core_static_root_symlink");
        let root = base.join("root");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).expect("failed to create static root");
        std::fs::create_dir_all(&outside).expect("failed to create outside dir");

        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "do not serve me").expect("failed to create outside file");

        let link = root.join("escape.txt");
        let _ = std::fs::remove_file(&link);
        if link_file(&secret, &link).is_err() {
            // Creating a symlink on Windows needs elevation, so an
            // unprivileged developer box cannot run the one assertion that
            // makes this test worth having. Skipping there is tolerable;
            // skipping *silently* is not, and skipping anywhere else is a bug.
            //
            // A test that returns `ok` without asserting is the same defect
            // `KRAB_REQUIRE_DB_TESTS` exists to prevent in `db_tests.rs`: the
            // gate stays green whether or not it verified anything. So the
            // skip is loud, and it is impossible off Windows — where CI runs.
            if !cfg!(windows) {
                panic!(
                    "symlink creation failed on a non-Windows host: this test asserted NOTHING and the traversal guard is unverified"
                );
            }
            eprintln!(
                "SKIPPED resolve_static_pkg_path_rejects_symlink_escaping_root: needs elevation on Windows; asserts on Linux CI"
            );
            return;
        }

        // Prove the link resolves before asserting it is refused — a dangling
        // link would make the assertion below pass for the wrong reason.
        assert_eq!(
            std::fs::canonicalize(&link).ok(),
            std::fs::canonicalize(&secret).ok(),
            "symlink did not resolve to the file outside the root"
        );

        assert!(resolve_static_pkg_path(&root, "escape.txt").is_none());
    }

    #[test]
    fn resolve_static_pkg_path_rejects_a_missing_file() {
        let root = std::env::temp_dir().join("krab_core_static_root_missing");
        std::fs::create_dir_all(&root).expect("failed to create static root");

        assert!(resolve_static_pkg_path(&root, "not-there.js").is_none());
    }

    #[test]
    fn static_mime_for_uses_lookup_and_safe_default() {
        assert_eq!(
            static_mime_for("/pkg/app.js"),
            "text/javascript".to_string()
        );
        assert_eq!(static_mime_for("/pkg/site.css"), "text/css".to_string());
        assert_eq!(
            static_mime_for("/pkg/index.html"),
            "text/html; charset=utf-8".to_string()
        );
        assert_eq!(
            static_mime_for("/pkg/module.wasm"),
            "application/wasm".to_string()
        );
        assert_eq!(
            static_mime_for("/pkg/blob.unknownext"),
            "application/octet-stream".to_string()
        );
    }

    #[test]
    fn normalize_static_mime_only_touches_html() {
        assert_eq!(
            normalize_static_mime("text/html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(normalize_static_mime("text/css"), "text/css");
        assert_eq!(
            normalize_static_mime("application/wasm"),
            "application/wasm"
        );
    }
}
