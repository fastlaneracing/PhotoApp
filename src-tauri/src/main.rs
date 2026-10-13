// Release builds are a GUI app: no console window when double-clicked.
// CLI subcommands re-attach to the parent terminal (see attach_parent_console).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod api;
mod db;
mod exif;
mod exifcheck;
mod history;
mod incremental;
mod library;
mod ops;
mod paths;
mod query;
mod scanner;
mod tauri_app;
mod thumbs;
mod watcher;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // No arguments (double-click / shortcut) or `app`: launch the Tauri GUI.
    if args.len() == 1 || args.iter().any(|a| a == "app") {
        tauri_app::run();
        return;
    }

    // Any CLI subcommand: print to the terminal that launched us.
    attach_parent_console();

    // `exifcheck` is a self-contained test against a fixed sample — no config/DB.
    if args.iter().any(|a| a == "exifcheck") {
        exifcheck::check();
        return;
    }

    // `api` runs the JSON dev server (browser mode of the Svelte UI).
    if args.iter().any(|a| a == "api") {
        let port = args
            .iter()
            .position(|a| a == "--port")
            .and_then(|i| args.get(i + 1))
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8787);
        api::serve(port);
        return;
    }

    let cfg_path_str = paths::config_path();
    let cfg_path = std::path::Path::new(&cfg_path_str);
    let db_path = paths::db_path();
    let db_dir = std::path::Path::new(&db_path).parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(db_dir).unwrap();

    let cfg = scanner::load_config(cfg_path);
    let mut conn = db::open(db_path.as_str());

    // One-shot sync with disk: new files cataloged, gone files marked
    // missing, moved paths pruned, duplicate groups rebuilt.
    if args.iter().any(|a| a == "rescan") {
        let summary = library::sync(&mut conn, &cfg);
        println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
        return;
    }

    // Catalog snapshot (data/backups/catalog-<stamp>.db).
    if args.iter().any(|a| a == "backup") {
        match library::backup(&conn) {
            Ok(p) => println!("backup: {p}"),
            Err(e) => println!("backup failed: {e}"),
        }
        return;
    }

    // M4: live watcher (Ctrl-C to stop). Prints each catalog mutation.
    if args.iter().any(|a| a == "watch") {
        println!("watching roots (Ctrl-C to stop)...");
        watcher::run_blocking(&cfg, |_cfg, action| {
            match action {
                watcher::WatchAction::Added(p) => {
                    let mut conn = db::open(db_path.as_str());
                    match incremental::process_file(&mut conn, &cfg, &p) {
                        Ok((mid, thumb)) => println!("  added {} (media {mid}, thumb {thumb})", p.display()),
                        Err(e) => println!("  FAIL {}: {e}", p.display()),
                    }
                }
                watcher::WatchAction::Rescan(p) => {
                    let mut conn = db::open(db_path.as_str());
                    let summary = library::sync(&mut conn, &cfg);
                    println!("  folder change {}: {summary}", p.display());
                }
                watcher::WatchAction::Removed(p) => {
                    let conn = db::open(db_path.as_str());
                    incremental::mark_missing(&conn, &p.to_string_lossy().to_string());
                    println!("  missing: {}", p.display());
                }
            }
        });
        return;
    }

    let want_thumbs = args.iter().any(|a| a == "thumbs");
    // Explicit `scan`: full walk + hash + EXIF pass. (No-arg now opens the GUI.)
    // `thumbs` alone: resumable fast path — operate on the existing catalog
    // WITHOUT re-scanning, so a re-run no longer re-hashes ~43 GB. This is
    // what made every launch timeout-prone.
    let want_scan = args.iter().any(|a| a == "scan");

    if want_scan {
        let t0 = std::time::Instant::now();
        let (media_rows, occ_rows, dup_groups) = scanner::scan(&mut conn, &cfg);
        let dt = t0.elapsed().as_secs_f64();
        println!("media rows (unique content): {media_rows}");
        println!("occurrence rows (paths): {occ_rows}");
        println!("duplicate groups: {dup_groups}");
        println!("scan time: {dt:.1}s");
    }

    if want_thumbs {
        let t1 = std::time::Instant::now();
        let s = thumbs::generate(&mut conn, std::path::Path::new(&paths::thumbs_dir()));
        let dt2 = t1.elapsed().as_secs_f64();
        println!(
            "thumbs: jobs {} | produced {} | already {} | images {} | videos {} | errors {}",
            s.jobs, s.produced, s.already, s.images, s.videos, s.errors.len()
        );
        for (sha, e) in s.errors.iter().take(15) {
            println!("  err {sha}: {e}");
        }
        println!("thumb time: {dt2:.1}s");
    }
}

/// Release builds use the Windows GUI subsystem, so they start with no console.
/// When run from cmd/PowerShell with a subcommand, attach to that terminal so
/// println! output is visible. No-op in debug builds and on other platforms.
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX; // (DWORD)-1
    unsafe extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// SQLite value -> JSON (for history before/after snapshots).
pub(crate) fn history_value(v: &rusqlite::types::Value) -> serde_json::Value {
    use rusqlite::types::Value as V;
    match v {
        V::Null => serde_json::Value::Null,
        V::Integer(i) => serde_json::json!(i),
        V::Real(f) => serde_json::json!(f),
        V::Text(t) => serde_json::json!(t),
        V::Blob(_) => serde_json::Value::Null,
    }
}
