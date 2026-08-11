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

fn collect_recursive(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            collect_recursive(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}
