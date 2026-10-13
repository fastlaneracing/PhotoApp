//! M4: live file watcher over the configured roots.
//!
//! `notify` (ReadDirectoryChangesW on Windows) with a 500 ms debounce.
//! Events are routed to a pluggable handler:
//!   - CLI `watch` prints to stdout (headless, testable),
//!   - Tauri app emits `catalog-changed` to the frontend.
//!
//! Create/modify of a media file  -> incremental::process_file
//! Remove of a media file         -> incremental::mark_missing
//! Rename (remove + create)       -> both, in debounce order; the create
//!                                   upserts the new path and re-flags present.
//! Folder created/moved/renamed/
//! deleted                          -> one library re-sync for the batch.
//!
//! Only media extensions (scanner::kind_of) are acted on; everything else is
//! noise. Excluded prefixes are filtered out here as well as in the walk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, Watcher};

use crate::scanner::Config;

const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq)]
pub enum WatchAction {
    Added(PathBuf),
    Removed(PathBuf),
    /// A folder was created, moved, renamed or deleted. Windows reports that as
    /// ONE event for the folder, not one per photo inside it, so the handler
    /// re-syncs the library (walk + diff; only new/changed files are hashed).
    Rescan(PathBuf),
}

/// Drive the watcher to completion on the calling thread. `on_action` is
/// invoked after debounce, once per changed path, in arrival order.
pub fn run_blocking(cfg: &Config, on_action: impl Fn(&Config, WatchAction)) {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res| { let _ = tx.send(res); },
        notify::Config::default(),
    )
    .expect("create watcher");

    for root in &cfg.roots {
        match watcher.watch(Path::new(&root.path), notify::RecursiveMode::Recursive) {
            Ok(()) => println!("watching: {}", root.path),
            Err(e) => eprintln!("watch failed on {}: {e}", root.path),
        }
    }

    // Debounce: collect path -> latest action until a quiet period elapses.
    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                let now = Instant::now();
                let mut pending: HashMap<PathBuf, WatchAction> = HashMap::new();
                for e in event.paths {
                    if let Some(action) = classify(&e, &event.kind, cfg) {
                        pending.insert(e, action);
                    }
                }
                // Wait out the debounce window for follow-up events (a rename
                // fires remove+create back-to-back; they must coalesce).
                let deadline = now + DEBOUNCE;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(remaining) {
                        Ok(Ok(e2)) => {
                            for e in e2.paths {
                                if let Some(action) = classify(&e, &e2.kind, cfg) {
                                    pending.insert(e, action);
                                }
                            }
                            continue;
                        }
                        _ => break, // quiet period elapsed (or channel error)
                    }
                }
                // Stable order: process removals before adds so a rename's
                // remove doesn't clobber the fresh upsert.
                let mut actions: Vec<WatchAction> = pending.into_values().collect();
                // One re-sync covers every file change in the batch.
                if let Some(r) = actions.iter().find(|a| matches!(a, WatchAction::Rescan(_))).cloned() {
                    actions = vec![r];
                }
                actions.sort_by_key(|a| match a {
                    WatchAction::Added(_) => 1,
                    WatchAction::Removed(_) => 0,
                    WatchAction::Rescan(_) => 2,
                });
                for action in actions {
                    on_action(cfg, action);
                }
            }
            // Channel closed (watcher dropped) -> stop.
            Ok(Err(_)) | Err(_) => break,
        }
    }
}

fn classify(path: &Path, kind: &EventKind, cfg: &Config) -> Option<WatchAction> {
    let s = path.to_string_lossy().to_lowercase();
    if cfg.exclusions.iter().any(|ex| s.starts_with(&ex.to_lowercase())) {
        return None;
    }
    use notify::event::RenameMode;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if crate::scanner::kind_of(&ext).is_none() {
        // Not a media file. A folder (existing, or a name with no extension
        // that just disappeared) means its contents moved: re-sync.
        let is_folderish = path.is_dir() || (!path.exists() && ext.is_empty());
        let structural = matches!(
            kind,
            EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
        );
        if is_folderish && structural {
            return Some(WatchAction::Rescan(path.to_path_buf()));
        }
        return None;
    }
    match kind {
        EventKind::Create(_) => Some(WatchAction::Added(path.to_path_buf())),
        EventKind::Remove(_) => Some(WatchAction::Removed(path.to_path_buf())),
        EventKind::Modify(notify::event::ModifyKind::Name(mode)) => {
            // A rename surfaces as a Name event. To = new path (exists),
            // From = old path (gone). Both/Other is ambiguous -> trust the disk.
            match mode {
                RenameMode::To => Some(WatchAction::Added(path.to_path_buf())),
                RenameMode::From => Some(WatchAction::Removed(path.to_path_buf())),
                _ => {
                    if path.exists() {
                        Some(WatchAction::Added(path.to_path_buf()))
                    } else {
                        Some(WatchAction::Removed(path.to_path_buf()))
                    }
                }
            }
        }
        EventKind::Modify(notify::event::ModifyKind::Any)
        | EventKind::Modify(notify::event::ModifyKind::Other) => {
            // In-place content change: re-run pipeline (hash/EXIF may differ).
            // Idempotent if the file is still there; harmless if not.
            Some(WatchAction::Added(path.to_path_buf()))
        }
        _ => None,
    }
}

/// One-shot convenience for the Tauri app: run the watcher on a background
/// thread and call `on_action` (which must be Send + 'static).
pub fn spawn_blocking(cfg: Config, on_action: impl Fn(&Config, WatchAction) + Send + 'static)
-> thread::JoinHandle<()> {
    thread::spawn(move || run_blocking(&cfg, on_action))
}
