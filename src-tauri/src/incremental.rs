//! M4: incremental catalog updates.
//!
//! `diff_scan` compares the on-disk tree against the DB and reports what is new
//! or missing. `process_file` catalogs one new/changed file through the same
//! pipeline as the bulk scan (hash + EXIF + upsert + thumbnail). The watcher
//! (watcher.rs) feeds these; the CLI `rescan` command exercises them headlessly.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use rusqlite::Connection;

use crate::scanner::{self, Config};
use crate::thumbs;

pub struct Diff {
    /// Files on disk (media extensions) with no occurrence row, plus known
    /// files whose mtime changed (edited in place).
    pub new_paths: Vec<std::path::PathBuf>,
    /// Occurrence rows (missing=0) whose file is gone from disk.
    pub missing_paths: Vec<String>,
    /// Roots that could not be read (drive unplugged / not mounted). Their
    /// rows are left untouched instead of being mass-marked missing.
    pub unreachable_roots: Vec<String>,
}

fn normalized_str(s: &str) -> String {
    crate::paths::key(s)
}

/// Walk configured roots and collect on-disk media files: map of
/// lowercase-path -> original-case PathBuf.
pub fn disk_map(cfg: &Config) -> std::collections::HashMap<String, std::path::PathBuf> {
    let mut m: std::collections::HashMap<String, std::path::PathBuf> = HashMap::new();
    for root in &cfg.roots {
        for p in scanner::walk(Path::new(&root.path), &cfg.exclusions) {
            let ext = p.extension().unwrap_or_default().to_string_lossy().to_lowercase();
            if scanner::kind_of(&ext).is_none() {
                continue;
            }
            if !p.is_file() {
                continue;
            }
            let key = normalized_str(&p.to_string_lossy().to_string());
            m.entry(key).or_insert_with(|| p.clone());
        }
    }
    m
}

/// Diff disk vs DB. Cheap: one directory walk (~0.3 s measured for the full
/// corpus), no hashing, no file reads.
pub fn diff_scan(conn: &Connection, cfg: &Config) -> Diff {
    let on_disk = disk_map(cfg);

    // Safety: a root that is not readable, or that walks to ZERO media files
    // while the catalog says it holds some, is treated as offline. Without
    // this, launching with E: unplugged marked the whole library missing.
    let mut unreachable: Vec<String> = Vec::new();
    for root in &cfg.roots {
        let readable = std::fs::read_dir(Path::new(&root.path)).is_ok();
        let rk = normalized_str(&root.path);
        let rk = rk.trim_end_matches('\\').to_string() + "\\";
        let found = on_disk.keys().any(|k| k.starts_with(&rk));
        let present_in_db: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM occurrence WHERE root = ?1 AND missing = 0",
                [&root.name],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if !readable || (!found && present_in_db > 0) {
            unreachable.push(root.name.clone());
        }
    }

    let mut missing = Vec::new();
    // Files edited while the app was closed: same path, different mtime.
    // They go through process_file again (re-hash; annotations carried over).
    let mut changed: Vec<std::path::PathBuf> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT path, root, mtime FROM occurrence WHERE missing = 0")
            .expect("query indexed paths");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
            .expect("iterate indexed paths");
        for (path, root, mtime) in rows.flatten() {
            if unreachable.contains(&root) {
                continue;
            }
            match on_disk.get(&normalized_str(&path)) {
                None => missing.push(path),
                Some(p) => {
                    let disk_mtime = fs::metadata(p)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64);
                    if let Some(dm) = disk_mtime {
                        if dm != mtime {
                            changed.push(p.clone());
                        }
                    }
                }
            }
        }
    }

    let known: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("SELECT path FROM occurrence").expect("query all paths");
        stmt.query_map([], |r| r.get::<_, String>(0))
            .expect("iterate all paths")
            .flatten()
            .map(|p| normalized_str(&p))
            .collect()
    };
    let mut new_paths: Vec<std::path::PathBuf> = on_disk
        .into_iter()
        .filter(|(k, _)| !known.contains(k))
        .map(|(_, p)| p)
        .collect();
    new_paths.extend(changed);

    Diff { new_paths, missing_paths: missing, unreachable_roots: unreachable }
}

pub fn normalized(p: &Path) -> String {
    normalized_str(&p.to_string_lossy().to_string())
}

/// Mark an occurrence as missing (file deleted from disk). The media row is
/// kept — identity is content, and the file may reappear.
pub fn mark_missing(conn: &Connection, path: &str) {
    let now = now_secs();
    let p = crate::paths::canon(path);
    // Never mark missing while the file is actually there (watcher events can
    // arrive out of order around renames / editor save-via-temp-file).
    if Path::new(&p).exists() {
        return;
    }
    conn.execute(
        "UPDATE occurrence SET missing = 1, indexed_at = ?1 WHERE path = ?2 COLLATE NOCASE",
        rusqlite::params![now, p],
    )
    .ok();
}

/// Catalog one new/changed file: pipeline (hash+EXIF) then thumbnail.
/// Returns (media_id, thumb_ok) so callers can report precisely.
pub fn process_file(conn: &mut Connection, cfg: &Config, path: &Path) -> Result<(i64, bool), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .ok_or_else(|| "no extension".to_string())?;
    let kind = scanner::kind_of(&ext)
        .ok_or_else(|| format!("unsupported extension: {ext}"))?;

    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    let bytes = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let root_name = cfg
        .roots
        .iter()
        .find(|r| normalized(path).starts_with(&normalized_str(&r.path)))
        .map(|r| r.name.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let entry = scanner::build_entry(path, &root_name, kind, &ext, bytes, mtime)
        .ok_or_else(|| "hash/EXIF pipeline failed".to_string())?;

    let media_id = {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let media_id = scanner::upsert_entry(&tx, &entry, now_secs())
            .ok_or_else(|| "upsert failed".to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        media_id
    };

    // Thumbnail — best effort: a corrupt video must not block the row.
    let thumb_dir_str = crate::paths::thumbs_dir();
    let thumb_dir = std::path::Path::new(&thumb_dir_str);
    fs::create_dir_all(&thumb_dir).ok();
    let out = thumb_dir.join(format!("{}.jpg", entry.sha));
    let thumb_ok = if out.exists() {
        true
    } else if kind == "image" {
        thumbs::make_image(&entry.path.to_string_lossy().to_string(), &out).is_ok()
    } else {
        thumbs::make_video(&entry.path.to_string_lossy().to_string(), &out).is_ok()
    };
    if thumb_ok {
        conn.execute(
            "UPDATE media SET thumb_path = ?1 WHERE sha256 = ?2",
            rusqlite::params![out.to_string_lossy().to_string(), entry.sha],
        )
        .ok();
    }

    Ok((media_id, thumb_ok))
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
