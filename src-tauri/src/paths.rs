//! Where photoApp keeps its files.
//!
//! The app root is found automatically: the first folder above the running
//! executable that contains a `config` folder (so `src-tauri\target\release\
//! photoapp.exe` inside a checkout uses the checkout), otherwise the folder the
//! executable is in. Everything app-owned lives under that root:
//!   config\config.toml   library roots + options (created on first run)
//!   data\catalog.db      the catalog (notes, tags, history...)
//!   data\thumbs\         thumbnail cache
//! Each can be overridden with PHOTOAPP_CONFIG / PHOTOAPP_DB / PHOTOAPP_THUMBS
//! (used by tests to run against a scratch catalog).

use std::path::PathBuf;

fn env_or(key: &str, default: String) -> String {
    std::env::var(key).unwrap_or(default)
}

/// The app root (see module docs). Computed once.
pub fn app_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir
            .ancestors()
            .find(|d| d.join("config").is_dir())
            .map(|d| d.to_path_buf())
            .unwrap_or(exe_dir)
    })
    .clone()
}

fn under_root(rel: &str) -> String {
    app_root().join(rel).to_string_lossy().to_string()
}

pub fn config_path() -> String {
    env_or("PHOTOAPP_CONFIG", under_root("config/config.toml"))
}

pub fn db_path() -> String {
    env_or("PHOTOAPP_DB", under_root("data/catalog.db"))
}

pub fn thumbs_dir() -> String {
    env_or("PHOTOAPP_THUMBS", under_root("data/thumbs"))
}

/// App data dir (parent of catalog.db): reports/, backups/, exports/ live here.
pub fn data_dir() -> std::path::PathBuf {
    let db = db_path();
    std::path::Path::new(&db)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The ONE stored form of a media path. Windows: backslashes only, no `\\?\`
/// prefix. Mixed forms (`D:/Pictures\x.jpg` from a walk vs `D:/Pictures/2010/x.jpg`
/// from a move) made a file the app moved look deleted + re-added on
/// the next rescan. Every write to `occurrence.path` goes through here.
pub fn canon(p: &str) -> String {
    let s = p.trim_start_matches(r"\\?\");
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s.to_string()
    }
}

/// Comparison key for paths (case-insensitive, separator-insensitive).
pub fn key(p: &str) -> String {
    p.trim_start_matches(r"\\?\").replace('/', "\\").to_lowercase()
}
