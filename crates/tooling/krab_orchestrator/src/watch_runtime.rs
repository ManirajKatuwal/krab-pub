use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::sync::mpsc;
use tracing::warn;

pub(super) struct EventWatchRuntime {
    pub(super) _watcher: RecommendedWatcher,
    pub(super) rx: mpsc::UnboundedReceiver<notify::Result<Event>>,
}

pub(super) fn watch_fingerprint(paths: &[String]) -> Result<u64> {
    let mut files = Vec::new();
    if paths.is_empty() {
        collect_recursive(Path::new("services/service_auth/src"), &mut files)?;
        collect_recursive(Path::new("services/service_users/src"), &mut files)?;
        collect_recursive(Path::new("services/service_frontend/src"), &mut files)?;
        collect_recursive(Path::new("crates/framework/krab_client/src"), &mut files)?;
    } else {
        for p in paths {
            collect_recursive(Path::new(p), &mut files)?;
        }
    }

    files.sort();
    let mut hasher = DefaultHasher::new();
    for file in files {
        file.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(&file) {
            if let Ok(modified) = meta.modified() {
                modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .hash(&mut hasher);
            }
            meta.len().hash(&mut hasher);
        }
    }

    Ok(hasher.finish())
}

pub(super) fn build_event_watch_runtime(paths: &[String]) -> Result<Option<EventWatchRuntime>> {
    let (tx, rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = match RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(err) => {
            warn!(error = %err, "event_watcher_initialization_failed");
            return Ok(None);
        }
    };

    let mut watched = 0_u64;
    for raw in paths {
        let path = PathBuf::from(raw);
        if !path.exists() {
            warn!(path = %path.display(), "watch_path_missing_skipping");
            continue;
        }
        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        match watcher.watch(path.as_path(), mode) {
            Ok(()) => {
                watched += 1;
            }
            Err(err) => {
                warn!(path = %path.display(), error = %err, "watch_registration_failed");
            }
        }
    }

    if watched == 0 {
        warn!("no_watch_paths_registered_falling_back_to_polling");
        return Ok(None);
    }

    Ok(Some(EventWatchRuntime {
        _watcher: watcher,
        rx,
    }))
}

/// Whether a changed path is editor or build noise that must not restart a
/// service.
///
/// Editors write swap, backup and atomic-save temp files beside the file being
/// edited (`.main.rs.swp`, `main.rs~`, `.#main.rs`, vim's `4913` probe,
/// JetBrains' `___jb_tmp___`), and each of those used to count as a source
/// change: one save restarted every watched service, sometimes twice. Build
/// and VCS directories (`target/`, `.git/`, `node_modules/`) are skipped
/// wholesale — a build writing into a watched tree must not trigger itself.
///
/// Only the part of `path` below the working directory is inspected for
/// ignored directory names, so a project that itself lives under a folder
/// called `target` is not ignored wholesale. Relative paths are inspected in
/// full.
pub(super) fn is_ignored_watch_path(path: &Path) -> bool {
    let cwd = std::env::current_dir().ok();
    let relative = cwd
        .as_deref()
        .and_then(|cwd| path.strip_prefix(cwd).ok())
        .unwrap_or(path);
    let inspect_dirs = !path.is_absolute() || relative != path;
    if inspect_dirs
        && relative
            .components()
            .any(|c| c.as_os_str().to_str().is_some_and(is_ignored_dir_name))
    {
        return true;
    }

    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_ignored_file_name)
}

/// Build, VCS and IDE directories whose contents are never source.
fn is_ignored_dir_name(name: &str) -> bool {
    matches!(
        name,
        "target" | ".git" | "node_modules" | ".idea" | ".vscode"
    )
}

/// Editor swap, backup and atomic-save temp files.
fn is_ignored_file_name(name: &str) -> bool {
    name.ends_with('~')
        || name.starts_with(".#")
        || (name.starts_with('#') && name.ends_with('#'))
        || name == "4913"
        || name == ".DS_Store"
        || name.contains("___jb_")
        || name.starts_with(".goutputstream-")
        || [".swp", ".swo", ".swx", ".tmp", ".bak", ".orig"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

/// Walk `path`, collecting every file beneath it that is not
/// [`is_ignored_watch_path`] noise.
///
/// Per-entry IO errors are logged and skipped rather than propagated. Scanning
/// a source tree races with whatever is writing to it — a `cargo build` or an
/// editor can remove a file between `read_dir` and the entry read — and a
/// fingerprint that fails on that turns a routine race into a supervisor
/// shutdown. A genuinely unreadable root still surfaces, via `read_dir` on the
/// top-level call.
fn collect_recursive(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "watch_scan_entry_skipped");
                continue;
            }
        };
        let p = entry.path();
        // Judged by the entry's own name: the walk descends from a root the
        // caller chose, so only names *below* it can be noise.
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (p.is_dir() && is_ignored_dir_name(&name)) || is_ignored_file_name(&name) {
            continue;
        }
        if p.is_dir() {
            if let Err(err) = collect_recursive(&p, out) {
                warn!(path = %p.display(), error = %err, "watch_scan_subtree_skipped");
            }
        } else {
            out.push(p);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{collect_recursive, is_ignored_watch_path, watch_fingerprint};
    use std::path::Path;

    #[test]
    fn editor_and_build_noise_is_ignored() {
        for noisy in [
            "src/.main.rs.swp",
            "src/main.rs.swo",
            "src/main.rs~",
            "src/.#main.rs",
            "src/#main.rs#",
            "src/4913",
            "src/.DS_Store",
            "src/main.rs___jb_tmp___",
            "src/main.rs.tmp",
            "services/x/target/debug/app",
            "services/x/.git/index",
            "web/node_modules/pkg/index.js",
        ] {
            assert!(is_ignored_watch_path(Path::new(noisy)), "{noisy}");
        }
        for real in [
            "src/main.rs",
            "src/routes/home.rs",
            "public/app.css",
            "Cargo.toml",
        ] {
            assert!(!is_ignored_watch_path(Path::new(real)), "{real}");
        }
    }

    /// An absolute path outside the working directory is judged by file name
    /// only, so a checkout under a folder named `target` still reloads.
    #[test]
    fn a_project_under_a_target_folder_is_not_ignored() {
        let outside = if cfg!(windows) {
            r"Z:\work\target\app\src\main.rs"
        } else {
            "/work/target/app/src/main.rs"
        };
        assert!(!is_ignored_watch_path(Path::new(outside)));
    }

    #[test]
    fn collect_recursive_skips_noise_and_finds_nested_sources() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("nested/lib.rs"), "").unwrap();
        std::fs::write(root.join(".main.rs.swp"), "x").unwrap();
        std::fs::write(root.join("target/debug/out"), "x").unwrap();

        let mut files = Vec::new();
        collect_recursive(root, &mut files).unwrap();
        files.sort();
        let names: Vec<_> = files
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(names, vec!["main.rs", "nested/lib.rs"]);
    }

    #[test]
    fn fingerprint_ignores_a_swap_file_but_sees_a_source_edit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().to_string();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        let paths = vec![root];

        let before = watch_fingerprint(&paths).unwrap();
        std::fs::write(dir.path().join(".main.rs.swp"), "swap").unwrap();
        assert_eq!(
            watch_fingerprint(&paths).unwrap(),
            before,
            "swap file ignored"
        );

        std::fs::write(dir.path().join("main.rs"), "fn main() { let _x = 1; }").unwrap();
        assert_ne!(watch_fingerprint(&paths).unwrap(), before, "edit detected");
    }

    #[test]
    fn missing_root_is_not_an_error() {
        let mut files = Vec::new();
        collect_recursive(Path::new("does/not/exist"), &mut files).unwrap();
        assert!(files.is_empty());
    }
}
