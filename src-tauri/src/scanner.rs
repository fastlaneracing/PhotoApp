use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::exif;

fn yes() -> bool {
    true
}

/// One library folder. Order in config.toml = priority (first = highest).
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Root {
    pub name: String,
    pub path: String,
    /// false = paused: not scanned or watched, its catalog rows are left as
    /// they are (like an unplugged drive).
    #[serde(default = "yes")]
    pub enabled: bool,
    /// true = photoApp never deletes, moves or replaces files in this folder
    /// (e.g. a backup drive). Duplicates there are always kept.
    #[serde(default)]
    pub protected: bool,
}

#[derive(Deserialize, Clone)]
pub struct Config {
    /// ACTIVE library folders only, in priority order. Paused folders are in
    /// `paused` so every walker / watcher skips them without extra checks.
    pub roots: Vec<Root>,
    #[serde(default)]
    pub exclusions: Vec<String>,
    /// ffmpeg.exe for video thumbnails (optional; default: `ffmpeg` on PATH).
    #[serde(default)]
    pub ffmpeg: Option<String>,
    #[serde(skip)]
    pub paused: Vec<Root>,
    /// Every folder as written in the file (active + paused), priority order.
    #[serde(skip)]
    pub all: Vec<Root>,
}

impl Config {
    /// Every configured folder (active and paused) in priority order.
    pub fn all_roots(&self) -> Vec<Root> {
        if self.all.is_empty() {
            let mut v = self.roots.clone();
            v.extend(self.paused.clone());
            v
        } else {
            self.all.clone()
        }
    }
    /// Priority index of a root by name (0 = highest).
    pub fn priority(&self, name: &str) -> usize {
        self.all_roots().iter().position(|r| r.name == name).unwrap_or(999)
    }
    pub fn is_protected(&self, name: &str) -> bool {
        self.roots.iter().chain(self.paused.iter()).any(|r| r.name == name && r.protected)
    }
    /// The root (active or paused) a path lives under; longest match wins.
    pub fn root_for_path(&self, path: &str) -> Option<&Root> {
        let k = crate::paths::key(path);
        self.roots
            .iter()
            .chain(self.paused.iter())
            .filter(|r| {
                let rk = crate::paths::key(&r.path);
                let rk = rk.trim_end_matches('\\');
                k == rk || k.starts_with(&format!("{rk}\\"))
            })
            .max_by_key(|r| r.path.len())
    }
}

/// The config written on first run: one library root = the user's Pictures folder.
fn default_config_text() -> String {
    let home = ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.trim().is_empty())
        .unwrap_or_else(|| ".".into());
    let pictures = Path::new(&home).join("Pictures").to_string_lossy().replace('\\', "/");
    format!(
        r#"# photoApp configuration - edit freely; restart the app after changes.
# Use forward slashes in paths, e.g. "D:/Pictures" (a \\ must be doubled in TOML).
# NOTE: top-level keys must come BEFORE the [[roots]] tables.

# Folders (path prefixes, case-insensitive) to skip while scanning.
exclusions = []

# Optional: full path to ffmpeg.exe for video thumbnails. Default: ffmpeg on PATH.
# ffmpeg = "C:/tools/ffmpeg/bin/ffmpeg.exe"

# One [[roots]] block per library folder, highest priority first. Manage them
# in the app under Settings (add, pause, protect, reorder).
[[roots]]
name = "Pictures"
path = "{pictures}"
"#
    )
}

pub fn load_config(path: &Path) -> Config {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, default_config_text());
        eprintln!("created default config at {} - edit it to point at your photo folders", path.display());
    }
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut cfg: Config =
        toml::from_str(&text).unwrap_or_else(|e| panic!("config error in {}: {e}", path.display()));
    let (active, paused): (Vec<Root>, Vec<Root>) = cfg.roots.drain(..).partition(|r| r.enabled);
    cfg.all = active.iter().chain(paused.iter()).cloned().collect();
    // Keep file order in `all` (partition preserved relative order per side).
    if let Ok(raw) = toml::from_str::<toml::Value>(&text) {
        if let Some(arr) = raw.get("roots").and_then(|v| v.as_array()) {
            let names: Vec<String> = arr.iter().filter_map(|r| r.get("name")?.as_str().map(String::from)).collect();
            cfg.all.sort_by_key(|r| names.iter().position(|n| n == &r.name).unwrap_or(usize::MAX));
        }
    }
    cfg.roots = active;
    cfg.paused = paused;
    cfg
}

const IMG: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tif", "tiff", "heic"];
const VID: &[&str] = &["mp4", "mov", "mkv", "avi", "m4v", "webm"];

pub(crate) fn kind_of(ext: &str) -> Option<&'static str> {
    if IMG.contains(&ext) { return Some("image"); }
    if VID.contains(&ext) { return Some("video"); }
    None
}

pub(crate) fn excluded(path: &str, exclusions: &[String]) -> bool {
    let p = path.to_lowercase();
    exclusions.iter().any(|e| p.starts_with(&e.to_lowercase()))
}

pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) root: String,
    pub(crate) kind: &'static str,
    pub(crate) ext: String,
    pub(crate) bytes: i64,
    pub(crate) mtime: i64,
    pub(crate) sha: String,
    pub(crate) taken_at: Option<String>,
    pub(crate) date_source: &'static str,
    pub(crate) camera: Option<String>,
    pub(crate) width: Option<i64>,
    pub(crate) height: Option<i64>,
    pub(crate) gps_lat: Option<f64>,
    pub(crate) gps_lon: Option<f64>,
}

pub(crate) fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// Date from filename: leading YYYY-MM-DD, optionally followed by _HHMMSS
// (or a space / 'T'), e.g. 2013-06-13_164706.jpg -> "2013-06-13 16:47:06";
// else a leading YYYY.
pub(crate) fn date_from_filename(name: &str) -> Option<String> {
    let c: Vec<char> = name.chars().collect();
    if c.len() < 4 { return None; }
    let y: String = c[..4].iter().collect();
    if !y.chars().all(|ch| ch.is_ascii_digit()) { return None; }
    let ynum: u32 = y.parse().ok()?;
    if ynum < 1970 || ynum > 2100 { return None; }
    let digits = |r: std::ops::Range<usize>| c[r].iter().all(|ch| ch.is_ascii_digit());
    if c.len() >= 10 && c[4] == '-' && c[7] == '-' && digits(5..7) && digits(8..10) {
        let base: String = c[..10].iter().collect();
        if c.len() >= 17 && matches!(c[10], '_' | ' ' | 'T') && digits(11..17) {
            let t: String = c[11..17].iter().collect();
            let (hh, mm, ss): (u32, u32, u32) = (t[..2].parse().ok()?, t[2..4].parse().ok()?, t[4..6].parse().ok()?);
            if hh < 24 && mm < 60 && ss < 60 {
                return Some(format!("{base} {}:{}:{}", &t[..2], &t[2..4], &t[4..6]));
            }
        }
        return Some(base);
    }
    Some(y)
}

#[cfg(test)]
mod tests {
    use super::date_from_filename as d;
    #[test]
    fn filename_dates() {
        assert_eq!(d("2013-06-13_164706.jpg").as_deref(), Some("2013-06-13 16:47:06"));
        assert_eq!(d("2013-06-13_164706_2.jpg").as_deref(), Some("2013-06-13 16:47:06"));
        assert_eq!(d("2013-06-13.jpg").as_deref(), Some("2013-06-13"));
        assert_eq!(d("2013-06-13_168999.jpg").as_deref(), Some("2013-06-13")); // not a time
        assert_eq!(d("2019 vacation.jpg").as_deref(), Some("2019"));
        assert_eq!(d("IMG_0001.jpg"), None);
    }
}

// Google Photos Takeout sidecar: metadata.json in the same folder.
pub(crate) fn date_from_sidecar(file_path: &Path) -> Option<String> {
    let sidecar = file_path.parent()?.join("metadata.json");
    let text = fs::read_to_string(&sidecar).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let t = v.get("mediaMetadata")?.get("creationTime")?.as_str()?;
    Some(t.to_string())
}

pub(crate) fn epoch_to_date(secs: i64) -> String {
    // Civil-from-days algorithm (Howard Hinnant), UTC.
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146097 } / 146097;
    let doe = (z - era * 146097).max(0);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146097) / 365;
    let y0 = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y0 + 1 } else { y0 };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:00", rem / 3600, (rem % 3600) / 60)
}

// Per-file pipeline: read once, hash, extract metadata (same fallback chain as
// the bulk scan). Shared by the bulk scan and the incremental (watcher) path so
// a live-added file is cataloged exactly like a scanned one.
pub(crate) fn build_entry(
    p: &Path,
    root: &str,
    kind: &'static str,
    ext: &str,
    bytes: i64,
    mtime: i64,
) -> Option<Entry> {
    let data = fs::read(p).ok()?;
    let mut h = Sha256::new();
    h.update(&data);
    let sha = hex(&h.finalize());

    let mut taken_at = None;
    let mut date_source = "mtime";
    let mut camera = None;
    let mut width = None;
    let mut height = None;
    let mut gps_lat = None;
    let mut gps_lon = None;

    if kind == "image" {
        let info = exif::parse(&data);
        if info.taken_at.is_some() {
            taken_at = info.taken_at;
            date_source = "exif";
        }
        camera = info.camera;
        width = info.width;
        height = info.height;
        gps_lat = info.gps_lat;
        gps_lon = info.gps_lon;
    }

    if taken_at.is_none() {
        if let Some(d) = date_from_sidecar(p) {
            taken_at = Some(d);
            date_source = "sidecar";
        }
    }
    if taken_at.is_none() {
        if let Some(d) = date_from_filename(p.file_name().unwrap_or_default().to_string_lossy().as_ref()) {
            taken_at = Some(d);
            date_source = "filename";
        }
    }
    if taken_at.is_none() {
        taken_at = Some(epoch_to_date(mtime));
        date_source = "mtime";
    }

    Some(Entry {
        path: p.to_path_buf(),
        root: root.to_string(),
        kind,
        ext: ext.to_string(),
        bytes,
        mtime,
        sha,
        taken_at,
        date_source,
        camera,
        width,
        height,
        gps_lat,
        gps_lon,
    })
}

// Upsert one entry into media + occurrence (idempotent on sha256/path).
//
// Cases:
//  * new path                       -> new occurrence row.
//  * known path, same content       -> refresh mtime, re-flag present.
//  * known path, DIFFERENT content  -> re-point the occurrence at the new
//    content and queue (old, new) in pending_carry. If the old content turns
//    up at another path it was a rename shuffle and its notes stay with it;
//    if it is gone, the file was edited in place and notes are carried to the
//    new version (library::resolve_pending_carry).
pub(crate) fn upsert_entry(
    tx: &rusqlite::Transaction,
    e: &Entry,
    now: i64,
) -> Option<i64> {
    let path_s = crate::paths::canon(&e.path.to_string_lossy());
    tx.execute(
        "INSERT OR IGNORE INTO media (sha256, kind, ext, bytes, width, height, taken_at, date_source, camera, gps_lat, gps_lon)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![e.sha, e.kind.to_string(), e.ext, e.bytes, e.width, e.height, e.taken_at, e.date_source.to_string(), e.camera, e.gps_lat, e.gps_lon],
    ).ok()?;
    let media_id: i64 = tx
        .query_row("SELECT id FROM media WHERE sha256 = ?1", rusqlite::params![e.sha], |r| r.get(0))
        .ok()?;

    // Back on disk (e.g. restored from the Recycle Bin): no longer deleted.
    tx.execute(
        "UPDATE media SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
        [media_id],
    ).ok()?;

    let existing: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, media_id FROM occurrence WHERE path = ?1 COLLATE NOCASE ORDER BY missing, id LIMIT 1",
            rusqlite::params![path_s],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();

    match existing {
        Some((occ_id, old_media)) if old_media != media_id => {
            tx.execute(
                "UPDATE occurrence SET media_id = ?2, path = ?3, mtime = ?4, indexed_at = ?5,
                        missing = 0, dup_group = NULL, is_canonical = 0, root = ?6
                 WHERE id = ?1",
                rusqlite::params![occ_id, media_id, path_s, e.mtime, now, e.root],
            ).ok()?;
            // Edit or rename-shuffle? Decided later, once all paths are known.
            tx.execute(
                "INSERT OR REPLACE INTO pending_carry (old_media, new_media, at) VALUES (?1, ?2, ?3)",
                rusqlite::params![old_media, media_id, now],
            ).ok()?;
        }
        Some((occ_id, _)) => {
            // The folder may have been renamed / re-added since: keep `root` current.
            tx.execute(
                "UPDATE occurrence SET path = ?2, mtime = ?3, indexed_at = ?4, missing = 0, root = ?5 WHERE id = ?1",
                rusqlite::params![occ_id, path_s, e.mtime, now, e.root],
            ).ok()?;
        }
        None => {
            tx.execute(
                "INSERT INTO occurrence (media_id, path, root, mtime, missing, indexed_at) VALUES (?1,?2,?3,?4,0,?5)",
                rusqlite::params![media_id, path_s, e.root, e.mtime, now],
            ).ok()?;
        }
    }
    Some(media_id)
}

pub fn scan(conn: &mut Connection, cfg: &Config) -> (usize, usize, usize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;

    // Pass 1: walk + stat (cheap).
    let mut raw: Vec<(PathBuf, String, &'static str, String, i64, i64)> = Vec::new();
    for root in &cfg.roots {
        for p in walk(&PathBuf::from(&root.path), &cfg.exclusions) {
            let ext = p.extension().unwrap_or_default().to_string_lossy().to_lowercase();
            let kind = match kind_of(&ext) { Some(k) => k, None => continue };
            let meta = match fs::metadata(&p) { Ok(m) => m, Err(_) => continue };
            if !meta.is_file() { continue; }
            let mtime = match meta.modified() {
                Ok(t) => t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64,
                Err(_) => 0,
            };
            raw.push((p, root.name.clone(), kind, ext, meta.len() as i64, mtime));
        }
    }

    // Pass 2: hash + metadata in parallel. One read per file serves both.
    let entries: Vec<Entry> = raw
        .par_iter()
        .filter_map(|(p, root, kind, ext, bytes, mtime)| {
            build_entry(p, root, *kind, ext, *bytes, *mtime)
        })
        .collect();

    // Pass 3: write catalog. media = unique content, occurrence = one row per path.
    // Incremental upsert keyed on sha256 / path (both UNIQUE in the schema): a re-run
    // against a populated DB no longer wipes rows first, so it avoids the FK constraint
    // failure that DELETE(media) -> DELETE(occurrence) caused, and preserves thumb_path
    // already written by a prior thumbs run. Duplicate flags are recomputed below from
    // the current occurrence set each run. INSERT OR IGNORE is safe here because every
    // path in this corpus is unique (no file appears under two roots), so an existing
    // media/occurrence row always means "already indexed" — nothing to update.
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0)).unwrap_or(0);
    let tx = conn.transaction().unwrap();
    let mut occ_rows = 0usize;
    for e in &entries {
        if upsert_entry(&tx, e, now).is_some() {
            occ_rows += 1;
        }
    }
    tx.commit().unwrap();
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0)).unwrap_or(0);
    let dup_groups = crate::library::recompute_dups(conn, cfg);
    ((after - before).max(0) as usize, occ_rows, dup_groups)
}

pub(crate) fn walk(dir: &Path, exclusions: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let ds = d.to_string_lossy().to_string();
        if excluded(&ds, exclusions) { continue; }
        match fs::read_dir(&d) {
            Ok(rd) => for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { stack.push(p); }
                else { out.push(p); }
            },
            Err(_) => {} // unreadable dir: skip, M1 reports via count check
        }
    }
    out
}
