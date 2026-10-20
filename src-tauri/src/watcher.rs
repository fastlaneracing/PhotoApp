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

/// Bumped when the library folders change (Settings); a running watcher
/// notices within a second and re-watches the new folder list.
pub static CONFIG_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn folders_changed() {
    CONFIG_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn watch_roots(watcher: &mut RecommendedWatcher, cfg: &Config, prev: &[String]) -> Vec<String> {
    let now: Vec<String> = cfg.roots.iter().map(|r| r.path.clone()).collect();
    for p in prev.iter().filter(|p| !now.contains(p)) {
        let _ = watcher.unwatch(Path::new(p));
        eprintln!("unwatched: {p}");
    }
    for p in now.iter().filter(|p| !prev.contains(p)) {
        match watcher.watch(Path::new(p), notify::RecursiveMode::Recursive) {
            Ok(()) => eprintln!("watching: {p}"),
            Err(e) => eprintln!("watch failed on {p}: {e}"),
        }
    }
    now
}

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
    run_reloading(cfg.clone(), None, on_action)
}

/// Like `run_blocking`; with `reload`, the folder list is re-read whenever
/// `folders_changed()` is called.
pub fn run_reloading(cfg: Config, reload: Option<fn() -> Config>, on_action: impl Fn(&Config, WatchAction)) {
    let mut cfg = cfg;
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res| { let _ = tx.send(res); },
        notify::Config::default(),
    )
    .expect("create watcher");

    let mut watched = watch_roots(&mut watcher, &cfg, &[]);
    let mut seen_gen = CONFIG_GEN.load(std::sync::atomic::Ordering::SeqCst);

    // Debounce: collect path -> latest action until a quiet period elapses.
    loop {
        if let Some(f) = reload {
            let g = CONFIG_GEN.load(std::sync::atomic::Ordering::SeqCst);
            if g != seen_gen {
                seen_gen = g;
                cfg = f();
                watched = watch_roots(&mut watcher, &cfg, &watched);
            }
        }
        let first = if reload.is_some() { rx.recv_timeout(Duration::from_secs(1)) } else { rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected) };
        match first {
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Err(_)) => break,
            Ok(Ok(event)) => {
                let now = Instant::now();
                let mut pending: HashMap<PathBuf, WatchAction> = HashMap::new();
                for e in event.paths {
                    if let Some(action) = classify(&e, &event.kind, &cfg) {
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
                                if let Some(action) = classify(&e, &e2.kind, &cfg) {
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
                    on_action(&cfg, action);
                }
            }
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
pub fn spawn_blocking(cfg: Config, reload: fn() -> Config, on_action: impl Fn(&Config, WatchAction) + Send + 'static)
-> thread::JoinHandle<()> {
    thread::spawn(move || run_reloading(cfg, Some(reload), on_action))
}
