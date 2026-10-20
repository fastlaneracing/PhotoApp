//! M5: Tauri 2 app — commands the Svelte frontend calls, plus the watcher
//! bridge that emits `catalog-changed` after any live catalog mutation.

use std::path::Path;

use tauri::Emitter;

use crate::scanner::{self, Config};
use crate::{incremental, watcher};

fn cfg() -> Config {
    let p = crate::paths::config_path();
    scanner::load_config(Path::new(&p))
}

fn open_db() -> rusqlite::Connection {
    let p = crate::paths::db_path();
    crate::db::open(p.as_str())
}

/// Grid payload: every item with a copy on disk, newest first.
#[tauri::command]
pub fn catalog_items() -> Result<Vec<crate::query::Item>, String> {
    let conn = open_db();
    crate::query::apply_filter(&conn, &crate::query::Filter::default())
}

#[tauri::command]
pub fn item_detail(id: i64) -> Result<crate::query::Detail, String> {
    let conn = open_db();
    crate::query::item_detail(&conn, id)
}

/// Library management: one command for tags, notes, ratings, albums, saved
/// searches, bulk moves and maintenance (see library::call for the list).
/// Runs on a worker thread (not the UI thread), so a full sync, cleanup or
/// 2,000-item bulk edit never freezes the window.
#[tauri::command]
pub async fn call(app: tauri::AppHandle, cmd: String, args: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let r = tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db();
        let c = cfg();
        crate::library::call(&mut conn, &c, &cmd, &args.unwrap_or(serde_json::json!({})))
    })
    .await
    .map_err(|e| e.to_string())??;
    // Library folders changed in Settings: let the viewer load the new
    // folders, re-point the live watcher, and catalog what's new.
    if r.get("folderChanged").and_then(|v| v.as_bool()) == Some(true) {
        grant_asset_scope(&app);
        watcher::folders_changed();
        if r.get("syncNeeded").and_then(|v| v.as_bool()) == Some(true) {
            background_sync(app.clone(), "folders changed");
        } else {
            let _ = app.emit("catalog-changed", serde_json::json!({ "note": "folders changed" }));
        }
    }
    Ok(r)
}

/// The webview may load files from the library folders and the thumbnail
/// cache only (nothing is hardcoded in tauri.conf.json).
fn grant_asset_scope(app: &tauri::AppHandle) {
    use tauri::Manager;
    let scope = app.asset_protocol_scope();
    let c = cfg();
    for r in c.roots.iter().chain(c.paused.iter()) {
        if let Err(e) = scope.allow_directory(&r.path, true) {
            eprintln!("asset scope {}: {e}", r.path);
        }
    }
    let thumbs = crate::paths::thumbs_dir();
    let _ = std::fs::create_dir_all(&thumbs);
    let _ = scope.allow_directory(&thumbs, true);
}

static SYNC_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SYNC_AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sync with disk on a background thread (progress -> `catalog-changed`), then
/// fingerprint new photos for duplicate detection and run the automatic
/// duplicate clean-up if Settings ask for it. One at a time; a request while
/// one runs makes it go round once more.
fn background_sync(handle: tauri::AppHandle, why: &'static str) {
    use std::sync::atomic::Ordering::SeqCst;
    if SYNC_BUSY.swap(true, SeqCst) {
        SYNC_AGAIN.store(true, SeqCst);
        return;
    }
    std::thread::spawn(move || {
        loop {
            SYNC_AGAIN.store(false, SeqCst);
            let cfg = cfg();
            let mut conn = open_db();
            if why == "launch sync" {
                crate::library::backup_if_due(&conn);
            }
            let h2 = handle.clone();
            let summary = crate::library::sync_with_progress(&mut conn, &cfg, &move |p| {
                let _ = h2.emit("catalog-changed", serde_json::json!({ "note": "sync progress", "progress": p }));
            });
            eprintln!("{why}: {summary}");
            let _ = handle.emit("catalog-changed", serde_json::json!({ "note": why, "sync": summary }));
            let h3 = handle.clone();
            crate::dedupe::fingerprint_pending(&conn, &move |done, total| {
                if total >= 50 && (done == total || done % 400 == 0) {
                    let p = serde_json::json!({ "phase": "fingerprint", "done": done, "total": if done == total { 0 } else { total } });
                    let _ = h3.emit("catalog-changed", serde_json::json!({ "note": "sync progress", "progress": p }));
                }
            });
            if let Some(d) = crate::dedupe::auto_resolve(&mut conn, &cfg) {
                eprintln!("auto duplicates: {d}");
                let _ = handle.emit("catalog-changed", serde_json::json!({ "note": "duplicates cleaned", "dedupe": d }));
            }
            // Refresh the header's duplicate count.
            let _ = crate::dedupe::scan(&conn, &cfg, false);
            let _ = handle.emit("catalog-changed", serde_json::json!({ "note": "duplicates counted" }));
            if !SYNC_AGAIN.load(SeqCst) {
                break;
            }
        }
        SYNC_BUSY.store(false, SeqCst);
    });
}

/// Native folder picker (Copy to…), parented to the app window so it stays on
/// top and modal. Returns { path } or { path: null } when cancelled.
#[tauri::command]
pub async fn pick_folder(window: tauri::Window, start: Option<String>, title: Option<String>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let d = crate::library::folder_dialog(start.as_deref(), title.as_deref().unwrap_or("Choose a folder"))
            .set_parent(&window);
        crate::library::pick_folder(d)
    })
    .await
    .map_err(|e| e.to_string())
}

/// Catalog statistics for the header bar.
#[tauri::command]
pub fn stats() -> Result<serde_json::Value, String> {
    let conn = open_db();
    Ok(crate::query::stats(&conn))
}

/// M6: apply a filter and return matching items (packaged-app path).
#[tauri::command]
pub async fn query(filter: crate::query::Filter) -> Result<Vec<crate::query::Item>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db();
        crate::query::apply_filter(&conn, &filter)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// M6: facet counts for the filter UI (packaged-app path).
#[tauri::command]
pub fn facets() -> Result<crate::query::Facets, String> {
    let conn = open_db();
    let cfg = cfg();
    Ok(crate::query::facets(&conn, &cfg))
}

// ---------------------------------------------------------------------------
// M7: duplicate view + flag/staging (non-destructive, charter-bound)
// ---------------------------------------------------------------------------

/// M7: all duplicate groups (largest content first).
#[tauri::command]
pub fn dup_groups(limit: Option<i64>) -> Result<Vec<crate::ops::DupGroup>, String> {
    let conn = open_db();
    crate::ops::dup_groups(&conn, limit)
}

/// M7: dup summary counts for the header.
#[tauri::command]
pub fn dup_summary() -> Result<crate::ops::DupSummary, String> {
    let conn = open_db();
    Ok(crate::ops::dup_summary(&conn))
}

/// M7: dry-run staging plan for one group (writes the report, moves nothing).
#[tauri::command]
pub fn staging_dry_run(media_id: i64) -> Result<crate::ops::StagingPlan, String> {
    let conn = open_db();
    let c = cfg();
    crate::ops::plan_staging(&conn, &c, media_id)
}

/// M7: apply staging for one group (moves files, flags, journals).
#[tauri::command]
pub fn staging_apply(media_id: i64) -> Result<crate::ops::StagingPlan, String> {
    let mut conn = open_db();
    let c = cfg();
    crate::ops::apply_staging(&mut conn, &c, media_id)
}

/// M7: undo the last applied staging for one group (reverse journal order).
#[tauri::command]
pub fn staging_undo(media_id: i64) -> Result<crate::ops::StagingPlan, String> {
    let mut conn = open_db();
    crate::ops::undo_staging(&mut conn, media_id)
}

// ---------------------------------------------------------------------------
// M8: rename / move (relocate) the canonical occurrence — non-destructive
// ---------------------------------------------------------------------------

/// M8: suggested move target for a media item (layout = year | year_month | root).
#[tauri::command]
pub fn move_target(media_id: i64, layout: Option<String>) -> Result<String, String> {
    let conn = open_db();
    let c = cfg();
    let layout = layout.unwrap_or_else(|| "year".to_string());
    crate::ops::move_target_for(&conn, &c, media_id, &layout)
}

/// M8: dry-run a rename/move (writes a report, moves nothing).
#[tauri::command]
pub fn relocate_dry_run(media_id: i64, op: String, target_path: String) -> Result<crate::ops::RelocatePlan, String> {
    let conn = open_db();
    crate::ops::plan_relocate(&conn, media_id, &target_path, &op)
}

/// M8: apply a rename/move (moves the canonical file, journals).
#[tauri::command]
pub fn relocate_apply(media_id: i64, op: String, target_path: String) -> Result<crate::ops::RelocatePlan, String> {
    let mut conn = open_db();
    crate::ops::apply_relocate(&mut conn, media_id, &target_path, &op)
}

/// M8: undo the last applied rename/move (reverse journal order).
#[tauri::command]
pub fn relocate_undo(media_id: i64) -> Result<crate::ops::RelocatePlan, String> {
    let mut conn = open_db();
    crate::ops::undo_relocate(&mut conn, media_id)
}

/// Live watcher: process each debounced action on the main thread's DB and
/// notify the frontend. Runs for the lifetime of the app (spawned in setup).
fn start_watcher(app: &tauri::AppHandle) {
    let app = std::sync::Arc::new(app.clone()); // Arc so each Fn call can clone an owned handle
    watcher::spawn_blocking(cfg(), cfg, move |cfg, action| {
        let app = app.clone(); // owned handle for this call
        // All DB + thumbnail work happens on this watcher thread; only the
        // final emit hops to the main thread.
        let result = (|| -> Result<String, String> {
            let mut conn = open_db();
            match &action {
                watcher::WatchAction::Added(p) => {
                    let (media_id, thumb_ok) = incremental::process_file(&mut conn, cfg, p)?;
                    // A moved file = new path for known content: drop the stale
                    // path row, refresh dup groups + search text for it.
                    crate::library::prune_moved(&conn);
                    crate::library::recompute_dups(&mut conn, cfg);
                    crate::query::fts_refresh(&conn, &[media_id]);
                    Ok(format!(
                        "added {} (media {media_id}, thumb {thumb_ok})",
                        p.to_string_lossy()
                    ))
                }
                watcher::WatchAction::Rescan(p) => {
                    let summary = crate::library::sync(&mut conn, cfg);
                    Ok(format!("folder change at {}: {summary}", p.to_string_lossy()))
                }
                watcher::WatchAction::Removed(p) => {
                    incremental::mark_missing(&conn, &p.to_string_lossy().to_string());
                    crate::library::recompute_dups(&mut conn, cfg);
                    Ok(format!(
                        "marked missing: {}",
                        p.to_string_lossy()
                    ))
                }
            }
        })();
        match result {
            Ok(msg) => {
                eprintln!("watcher: {msg}");
                let _ = app.emit("catalog-changed", serde_json::json!({ "note": msg }));
            }
            Err(e) => {
                eprintln!("watcher: action failed: {e}");
                let _ = app.emit("catalog-changed", serde_json::json!({ "note": format!("error: {e}") }));
            }
        }
    });
}

/// Launch the Tauri GUI. Called from `main` when argv has `app`.
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            catalog_items,
            item_detail,
            call,
            pick_folder,
            stats,
            query,
            facets,
            dup_groups,
            dup_summary,
            staging_dry_run,
            staging_apply,
            staging_undo,
            move_target,
            relocate_dry_run,
            relocate_apply,
            relocate_undo
        ])
        .setup(|app| {
            grant_asset_scope(app.handle());
            // Launch sync on a background thread so the window paints at once
            // even when many files were added while the app was closed. The
            // grid refreshes on the `catalog-changed` event when it finishes.
            background_sync(app.handle().clone(), "launch sync");
            start_watcher(app.handle());
            // Settle edit-vs-rename decisions from the live watcher once a
            // burst of file changes has gone quiet for a while.
            std::thread::spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let mut conn = open_db();
                crate::library::resolve_pending_carry(&mut conn, 20);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run tauri app");
}
