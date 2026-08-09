//! Serving static assets safely.
//!
//! Ported from `krab_server` when that crate was removed (see
//! [ADR 0005](https://github.com/krab-framework/krab/blob/main/docs/adr/0005-krab-server-disposition.md)).
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

    let canonical_root = std::fs::canonicalize(static_root).ok()?;
    let candidate = canonical_root.join(requested);
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
