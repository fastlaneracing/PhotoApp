//! Duplicates across all library folders.
//!
//! Two kinds of duplicate are found:
//!   * identical copies: the same bytes (same SHA-256) at more than one path;
//!   * the same photo saved differently: resized, re-compressed, converted or
//!     stripped of its camera data. Found with a 64-bit picture fingerprint
//!     (difference hash of the thumbnail, EXIF rotation applied) plus checks on
//!     shape and date taken.
//!
//! In every group one copy is kept: the highest quality one (most pixels, then
//! lossless over lossy, then camera data, then file size), or the copy in the
//! highest-priority folder, depending on Settings. The others go to the Recycle
//! Bin or to a review folder outside the library. Every clean-up is a "run"
//! that can be undone as a whole; notes, tags, rating and albums of removed
//! copies are merged into the kept one (recorded in the change history, so the
//! undo reverts them too).
//!
//! Look-alikes (same size, same format, both with camera data: burst shots,
//! light edits) are listed as "similar" for review and are never removed
//! automatically. Files in protected folders are never touched.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::scanner::Config;
use crate::settings::{dup_settings, DupSettings};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dedupe_runs (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  action TEXT NOT NULL,              -- recycle | move
  auto INTEGER NOT NULL DEFAULT 0,
  groups INTEGER NOT NULL DEFAULT 0,
  files INTEGER NOT NULL DEFAULT 0,
  bytes INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'done', -- done | undone
  undone_at INTEGER
);
CREATE TABLE IF NOT EXISTS dedupe_log (
  id INTEGER PRIMARY KEY,
  run_id INTEGER NOT NULL,
  media_id INTEGER NOT NULL,
  occ_id INTEGER NOT NULL,
  keeper_media INTEGER NOT NULL,
  path TEXT NOT NULL,
  new_path TEXT,
  bytes INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,              -- done | failed | undone
  error TEXT
);
CREATE INDEX IF NOT EXISTS idx_dedupe_log_run ON dedupe_log(run_id);
-- Groups the user marked as \"not duplicates\" (signature = member media ids).
CREATE TABLE IF NOT EXISTS dup_ignore (sig TEXT PRIMARY KEY, at INTEGER NOT NULL);
";

// ---------------------------------------------------------------------------
// Picture fingerprints
// ---------------------------------------------------------------------------

/// Difference hash: 9x8 grayscale, one bit per horizontal neighbour pair.
pub fn dhash(img: &image::DynamicImage) -> u64 {
    let g = img.grayscale().resize_exact(9, 8, image::imageops::FilterType::Triangle).to_luma8();
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h <<= 1;
            if g.get_pixel(x, y)[0] < g.get_pixel(x + 1, y)[0] {
                h |= 1;
            }
        }
    }
    h
}

/// (oriented width, oriented height, fingerprint) for one image. Dimensions
/// come from the original's header (no full decode); the fingerprint from the
/// cached thumbnail, turned upright with the original's EXIF orientation.
fn fingerprint(original: Option<&str>, thumb: Option<&str>) -> (Option<i64>, Option<i64>, Option<i64>) {
    use image::metadata::Orientation;
    use image::ImageDecoder;
    let mut orient = Orientation::NoTransforms;
    let mut dims: Option<(u32, u32)> = None;
    if let Some(p) = original {
        if let Ok(r) = image::ImageReader::open(p).and_then(|r| r.with_guessed_format()) {
            if let Ok(mut dec) = r.into_decoder() {
                dims = Some(dec.dimensions());
                orient = dec.orientation().unwrap_or(Orientation::NoTransforms);
            }
        }
    }
    let swap = matches!(
        orient,
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
    );
    let (w, h) = match dims {
        Some((w, h)) if swap => (Some(h as i64), Some(w as i64)),
        Some((w, h)) => (Some(w as i64), Some(h as i64)),
        None => (None, None),
    };
    let src = thumb.filter(|t| Path::new(t).is_file()).or(original);
    let hash = src.and_then(|p| {
        let mut img = image::ImageReader::open(p).ok()?.with_guessed_format().ok()?.decode().ok()?;
        img.apply_orientation(orient);
        Some(dhash(&img) as i64)
    });
    (w, h, hash)
}

/// Fingerprint every on-disk image that has none yet (content never changes
/// for a given row, so each image is done once). Returns how many were done.
pub fn fingerprint_pending(conn: &Connection, progress: &dyn Fn(usize, usize)) -> usize {
    // One pass at a time (the background sync and the Duplicates tab can both
    // ask); a second caller waits and then finds nothing left to do.
    static BUSY: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = BUSY.lock().unwrap_or_else(|p| p.into_inner());
    let rows: Vec<(i64, Option<String>, Option<String>)> = conn
        .prepare(
            "SELECT m.id, m.thumb_path,
                    (SELECT o.path FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0
                      ORDER BY o.is_canonical DESC, o.id LIMIT 1)
             FROM media m
             WHERE m.kind = 'image' AND m.fp_at IS NULL
               AND EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    let total = rows.len();
    let mut done = 0;
    for chunk in rows.chunks(400) {
        let out: Vec<(i64, (Option<i64>, Option<i64>, Option<i64>))> = chunk
            .par_iter()
            .map(|(id, thumb, path)| (*id, fingerprint(path.as_deref(), thumb.as_deref())))
            .collect();
        if let Ok(tx) = conn.unchecked_transaction() {
            for (id, (w, h, fp)) in &out {
                let _ = tx.execute(
                    "UPDATE media SET px_w = ?2, px_h = ?3, phash = ?4, fp_at = ?5 WHERE id = ?1",
                    params![id, w, h, fp, now()],
                );
            }
            let _ = tx.commit();
        }
        done += chunk.len();
        progress(done, total);
    }
    total
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Copy {
    pub occ_id: i64,
    pub media_id: i64,
    pub path: String,
    pub root: String,
    pub kind: String,
    pub ext: String,
    pub bytes: i64,
    pub w: Option<i64>,
    pub h: Option<i64>,
    pub taken_at: Option<String>,
    pub date_source: Option<String>,
    pub camera: Option<String>,
    pub thumb: Option<String>,
    pub phash: Option<i64>,
    pub priority: usize,
    pub protected: bool,
}

impl Copy {
    fn pixels(&self) -> i64 {
        self.w.unwrap_or(0) * self.h.unwrap_or(0)
    }
    fn has_camera(&self) -> bool {
        self.camera.as_deref().map(|c| !c.trim().is_empty()).unwrap_or(false) || self.date_source.as_deref() == Some("exif")
    }
    fn name(&self) -> &str {
        self.path.rsplit(['\\', '/']).next().unwrap_or(&self.path)
    }
    /// Dates good enough to compare: from the camera or the file name.
    fn solid_date(&self) -> Option<&str> {
        match self.date_source.as_deref() {
            Some("exif") | Some("filename") => self.taken_at.as_deref().map(|d| &d[..d.len().min(19)]),
            _ => None,
        }
    }
}

/// Higher = better format for keeping. Lossless beats lossy at the same size.
fn format_rank(ext: &str) -> i32 {
    match ext {
        "tif" | "tiff" => 6,
        "png" => 5,
        "heic" => 4,
        "jpg" | "jpeg" => 3,
        "webp" => 2,
        "bmp" => 2,
        "gif" => 1,
        _ => 0,
    }
}
fn lossless(ext: &str) -> bool {
    matches!(ext, "tif" | "tiff" | "png")
}
fn format_label(ext: &str) -> String {
    match ext {
        "jpg" | "jpeg" => "JPEG".into(),
        "tif" | "tiff" => "TIFF".into(),
        e => e.to_uppercase(),
    }
}

/// "IMG_1234 (2).jpg", "Copy of IMG_1234.jpg", "IMG_1234 - Copy.jpg"…
pub fn looks_like_a_copy(name: &str) -> bool {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name).to_lowercase();
    let stem = stem.trim();
    if stem.starts_with("copy of ") || stem.ends_with(" copy") || stem.ends_with("- copy") || stem.ends_with("_copy") || stem.ends_with("(copy)") {
        return true;
    }
    if let Some(open) = stem.rfind(" (") {
        let inner = &stem[open + 2..];
        if let Some(n) = inner.strip_suffix(')') {
            return !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

fn pixels_cmp(a: &Copy, b: &Copy) -> Ordering {
    let (pa, pb) = (a.pixels() as f64, b.pixels() as f64);
    if pa > pb * 1.01 {
        Ordering::Greater
    } else if pb > pa * 1.01 {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

/// Pixel count in ~1 % steps. Sorting needs a TOTAL order: "within 1 % of
/// each other" is not transitive (A≈B, B≈C but A>C), and Rust's sort panics
/// ("comparison function does not correctly implement a total order") when it
/// notices. Buckets keep near-equal sizes equal and stay consistent.
fn pixel_bucket(c: &Copy) -> i64 {
    let p = c.pixels();
    if p <= 0 { 0 } else { ((p as f64).ln() / 1.01f64.ln()).floor() as i64 + 1 }
}

/// Picture quality only (Greater = a is better).
fn quality_cmp(a: &Copy, b: &Copy) -> Ordering {
    // Camera data before format: a PNG converted from a camera JPEG has lost
    // the original's date and camera; an edit that kept them (TIFF) still wins.
    pixel_bucket(a).cmp(&pixel_bucket(b))
        .then(a.has_camera().cmp(&b.has_camera()))
        .then(format_rank(&a.ext).cmp(&format_rank(&b.ext)))
        .then(a.bytes.cmp(&b.bytes))
}

/// Which copy to keep (Greater = keep a rather than b).
fn keep_cmp(a: &Copy, b: &Copy, s: &DupSettings) -> Ordering {
    let names = |x: &Copy, y: &Copy| {
        if s.prefer_original_names {
            (!looks_like_a_copy(x.name())).cmp(&!looks_like_a_copy(y.name()))
        } else {
            Ordering::Equal
        }
    };
    let folder = |x: &Copy, y: &Copy| y.priority.cmp(&x.priority); // lower index = better
    let tail = |x: &Copy, y: &Copy| y.path.len().cmp(&x.path.len()).then(y.occ_id.cmp(&x.occ_id));
    if s.keep_rule == "folder" {
        folder(a, b).then(quality_cmp(a, b)).then(names(a, b)).then(tail(a, b))
    } else {
        quality_cmp(a, b).then(names(a, b)).then(folder(a, b)).then(tail(a, b))
    }
}

/// Plain-words reason the keeper won against the runner-up.
fn keep_reason(k: &Copy, other: &Copy, s: &DupSettings, exact: bool) -> String {
    let folder = || format!("in your highest-priority folder ({})", k.root);
    if s.keep_rule == "folder" && k.priority < other.priority {
        return folder();
    }
    if !exact {
        match pixels_cmp(k, other) {
            Ordering::Greater => {
                return format!("largest picture ({}×{} vs {}×{})", k.w.unwrap_or(0), k.h.unwrap_or(0), other.w.unwrap_or(0), other.h.unwrap_or(0))
            }
            Ordering::Less => {}
            Ordering::Equal => {
                if k.has_camera() && !other.has_camera() {
                    return "keeps the camera data (date, camera, settings)".into();
                }
                if format_rank(&k.ext) > format_rank(&other.ext) {
                    return if lossless(&k.ext) && !lossless(&other.ext) {
                        format!("lossless {} instead of {}", format_label(&k.ext), format_label(&other.ext))
                    } else {
                        format!("better format ({} vs {})", format_label(&k.ext), format_label(&other.ext))
                    };
                }
                if k.has_camera() && !other.has_camera() {
                    return "keeps the camera data (date, camera, settings)".into();
                }
                if k.bytes > other.bytes {
                    return format!("least compressed ({} vs {})", fmt_bytes(k.bytes), fmt_bytes(other.bytes));
                }
            }
        }
    }
    if s.prefer_original_names && !looks_like_a_copy(k.name()) && looks_like_a_copy(other.name()) {
        return "original file name".into();
    }
    if k.priority < other.priority {
        return folder();
    }
    if exact { "identical copies — first by folder order and path".into() } else { "best overall".into() }
}

fn fmt_bytes(b: i64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{} KB", (b / 1e3).round().max(1.0))
    }
}

/// How a non-kept copy relates to the kept one.
fn relation(k: &Copy, o: &Copy) -> &'static str {
    if k.media_id == o.media_id {
        return "identical";
    }
    if pixels_cmp(k, o) == Ordering::Greater {
        return "lower";
    }
    if pixels_cmp(k, o) == Ordering::Equal {
        if lossless(&k.ext) && !lossless(&o.ext) {
            return "lower";
        }
        if k.has_camera() && !o.has_camera() {
            return "lower";
        }
        if !k.has_camera() && !o.has_camera() && (o.bytes as f64) < k.bytes as f64 * 0.85 {
            return "lower";
        }
    }
    "lookalike"
}

fn load_copies(conn: &Connection, cfg: &Config) -> Vec<Copy> {
    let active: HashSet<&str> = cfg.roots.iter().map(|r| r.name.as_str()).collect();
    conn.prepare(
        "SELECT o.id, o.media_id, o.path, o.root, m.kind, m.ext, m.bytes,
                COALESCE(m.px_w, m.width), COALESCE(m.px_h, m.height),
                m.taken_at, m.date_source, m.camera, m.thumb_path, m.phash
         FROM occurrence o JOIN media m ON m.id = o.media_id
         WHERE o.missing = 0
         ORDER BY o.media_id, o.id",
    )
    .and_then(|mut st| {
        st.query_map([], |r| {
            Ok(Copy {
                occ_id: r.get(0)?,
                media_id: r.get(1)?,
                path: r.get(2)?,
                root: r.get(3)?,
                kind: r.get(4)?,
                ext: r.get::<_, String>(5)?.to_lowercase(),
                bytes: r.get(6)?,
                w: r.get(7)?,
                h: r.get(8)?,
                taken_at: r.get(9)?,
                date_source: r.get(10)?,
                camera: r.get(11)?,
                thumb: r.get(12)?,
                phash: r.get(13)?,
                priority: 0,
                protected: false,
            })
        })
        .map(|it| it.flatten().collect::<Vec<_>>())
    })
    .unwrap_or_default()
    .into_iter()
    .filter(|c: &Copy| active.contains(c.root.as_str()))
    .map(|mut c| {
        c.priority = cfg.priority(&c.root);
        c.protected = cfg.is_protected(&c.root);
        c
    })
    .collect()
}

struct Uf(Vec<usize>);
impl Uf {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut c = x;
        while self.0[c] != r {
            let n = self.0[c];
            self.0[c] = r;
            c = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra.max(rb)] = ra.min(rb);
        }
    }
}

fn aspect(c: &Copy) -> Option<f64> {
    match (c.w, c.h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f64 / h as f64),
        _ => None,
    }
}

/// Could two different files be the same picture?
fn similar_pair(a: &Copy, b: &Copy, s: &DupSettings) -> Option<u32> {
    let (ha, hb) = (a.phash?, b.phash?);
    let d = ((ha ^ hb) as u64).count_ones();
    if d > s.max_distance() {
        return None;
    }
    // Same shape (a crop is a different picture).
    if let (Some(x), Some(y)) = (aspect(a), aspect(b)) {
        if (x / y - 1.0).abs() > 0.03 {
            return None;
        }
    }
    if s.require_same_date {
        if let (Some(x), Some(y)) = (a.solid_date(), b.solid_date()) {
            if x != y {
                return None;
            }
        }
    }
    Some(d)
}

fn hamming(a: Option<i64>, b: Option<i64>) -> Option<u32> {
    Some(((a? ^ b?) as u64).count_ones())
}

/// Every duplicate group, largest reclaimable space first.
pub fn find_groups(conn: &Connection, cfg: &Config, s: &DupSettings, include_ignored: bool) -> Vec<Value> {
    let copies = load_copies(conn, cfg);
    // media id -> copy indexes
    let mut by_media: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, c) in copies.iter().enumerate() {
        if c.kind == "video" && !s.include_videos {
            continue;
        }
        by_media.entry(c.media_id).or_default().push(i);
    }
    let mut media_ids: Vec<i64> = by_media.keys().copied().collect();
    media_ids.sort_unstable();
    let mut uf = Uf((0..media_ids.len()).collect());

    // Similar pictures (images only, one representative per content).
    if s.similar {
        let reps: Vec<(usize, &Copy)> = media_ids
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let c = &copies[by_media[m][0]];
                (c.kind == "image" && c.phash.is_some()).then_some((i, c))
            })
            .collect();
        let pairs: Vec<(usize, usize)> = reps
            .par_iter()
            .enumerate()
            .flat_map_iter(|(k, (i, a))| {
                reps[k + 1..]
                    .iter()
                    .filter(move |(_, b)| similar_pair(a, b, s).is_some())
                    .map(move |(j, _)| (*i, *j))
            })
            .collect();
        for (i, j) in pairs {
            uf.union(i, j);
        }
    }

    let mut comps: HashMap<usize, Vec<i64>> = HashMap::new();
    for (i, m) in media_ids.iter().enumerate() {
        comps.entry(uf.find(i)).or_default().push(*m);
    }

    let ignored: HashSet<String> = if include_ignored {
        HashSet::new()
    } else {
        conn.prepare("SELECT sig FROM dup_ignore")
            .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()))
            .unwrap_or_default()
    };

    let mut out: Vec<Value> = vec![];
    let mut work: Vec<Vec<i64>> = comps.into_values().collect();
    while let Some(mut medias) = work.pop() {
        medias.sort_unstable();
        let mut members: Vec<&Copy> = medias.iter().flat_map(|m| by_media[m].iter().map(|i| &copies[*i])).collect();
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| keep_cmp(b, a, s)); // best first
        let keeper = members[0];
        // Every picture must be close to the KEEPER (no chains of look-alikes).
        let far: Vec<i64> = medias
            .iter()
            .copied()
            .filter(|m| *m != keeper.media_id)
            .filter(|m| {
                let c = &copies[by_media[m][0]];
                hamming(keeper.phash, c.phash).map(|d| d > s.max_distance()).unwrap_or(true)
            })
            .collect();
        if !far.is_empty() {
            let near: Vec<i64> = medias.iter().copied().filter(|m| !far.contains(m)).collect();
            work.push(near);
            work.push(far);
            continue;
        }
        let roots: HashSet<&str> = members.iter().map(|c| c.root.as_str()).collect();
        if s.scope == "acrossFolders" && roots.len() < 2 {
            continue;
        }
        let sig = if medias.len() == 1 { format!("e:{}", medias[0]) } else { medias.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(",") };
        if ignored.contains(&sig) {
            continue;
        }
        let exact = medias.len() == 1;
        let rels: Vec<&str> = members[1..].iter().map(|c| relation(keeper, c)).collect();
        let kind = if exact {
            "exact"
        } else if rels.iter().all(|r| *r != "lookalike") {
            "copies"
        } else {
            "similar"
        };
        // Nothing can be removed (every other copy is protected): not actionable.
        if members[1..].iter().all(|c| c.protected) {
            continue;
        }
        let reclaim: i64 = members[1..].iter().filter(|c| !c.protected).map(|c| c.bytes).sum();
        let mut root_names: Vec<&str> = roots.into_iter().collect();
        root_names.sort_by_key(|r| cfg.priority(r));
        let member_json: Vec<Value> = members
            .iter()
            .enumerate()
            .map(|(i, c)| {
                json!({
                    "occId": c.occ_id, "mediaId": c.media_id, "path": c.path, "name": c.name(),
                    "root": c.root, "rootPriority": c.priority + 1, "protected": c.protected,
                    "kind": c.kind, "ext": c.ext, "format": format_label(&c.ext), "bytes": c.bytes,
                    "width": c.w, "height": c.h, "takenAt": c.taken_at, "camera": c.camera,
                    "hasCamera": c.has_camera(), "thumbPath": c.thumb,
                    "keep": i == 0,
                    "relation": if i == 0 { "keeper" } else { rels[i - 1] },
                    "distance": hamming(keeper.phash, c.phash),
                    "copyName": looks_like_a_copy(c.name()),
                })
            })
            .collect();
        // Explain against the best DIFFERENT picture when there is one
        // (identical copies only differ by name / folder).
        let other = members[1..].iter().find(|c| c.media_id != keeper.media_id).copied().unwrap_or(members[1]);
        let reason = keep_reason(keeper, other, s, other.media_id == keeper.media_id);
        out.push(json!({
            "id": sig,
            "kind": kind,
            "keepOccId": keeper.occ_id,
            "reason": reason,
            "title": keeper.name(),
            "takenAt": keeper.taken_at,
            "mediaKind": keeper.kind,
            "thumbPath": keeper.thumb,
            "roots": root_names,
            "reclaimBytes": reclaim,
            "removable": members[1..].iter().filter(|c| !c.protected).count(),
            "members": member_json,
        }));
    }
    out.sort_by(|a, b| {
        b["reclaimBytes"].as_i64().cmp(&a["reclaimBytes"].as_i64()).then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    out
}

fn summarize(groups: &[Value]) -> Value {
    let mut by: HashMap<&str, (i64, i64, i64)> = HashMap::new();
    for g in groups {
        let e = by.entry(g["kind"].as_str().unwrap_or("")).or_default();
        e.0 += 1;
        e.1 += g["removable"].as_i64().unwrap_or(0);
        e.2 += g["reclaimBytes"].as_i64().unwrap_or(0);
    }
    let k = |n: &str| {
        let (g, f, b) = by.get(n).copied().unwrap_or_default();
        json!({ "groups": g, "files": f, "bytes": b })
    };
    json!({
        "groups": groups.len(),
        "exact": k("exact"), "copies": k("copies"), "similar": k("similar"),
        "bytes": groups.iter().map(|g| g["reclaimBytes"].as_i64().unwrap_or(0)).sum::<i64>(),
    })
}

/// Fingerprint what's new, then list groups. Caches the counts for the header.
pub fn scan(conn: &Connection, cfg: &Config, include_ignored: bool) -> Value {
    let t0 = std::time::Instant::now();
    let fingerprinted = fingerprint_pending(conn, &|_, _| {});
    let s = dup_settings(conn);
    let groups = find_groups(conn, cfg, &s, include_ignored);
    let summary = summarize(&groups);
    let _ = conn.execute(
        "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('dup_summary_cache', ?1)",
        [summary.to_string()],
    );
    let ignored: i64 = conn.query_row("SELECT COUNT(*) FROM dup_ignore", [], |r| r.get(0)).unwrap_or(0);
    json!({
        "groups": groups,
        "summary": summary,
        "settings": s,
        "fingerprinted": fingerprinted,
        "ignored": ignored,
        "seconds": t0.elapsed().as_secs_f64(),
    })
}

// ---------------------------------------------------------------------------
// Resolve + undo
// ---------------------------------------------------------------------------

/// Move a file, across drives if needed (copy, verify size, remove source).
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    if to.exists() {
        return Err(format!("{} already exists", to.display()));
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let n = std::fs::copy(from, to).map_err(|e| format!("copy failed: {e}"))?;
    let src_len = std::fs::metadata(from).map(|m| m.len()).unwrap_or(u64::MAX);
    if n != src_len {
        let _ = std::fs::remove_file(to);
        return Err("copy is incomplete; original left in place".into());
    }
    if let Ok(m) = std::fs::metadata(from).and_then(|m| m.modified()) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(to) {
            let _ = f.set_modified(m);
        }
    }
    std::fs::remove_file(from).map_err(|e| format!("copied, but could not remove the original: {e}"))
}

/// Where a removed copy goes in "move" mode: <move dir>\<folder name>\<path
/// inside that folder>, with " (2)" etc. if that name is taken.
fn review_target(cfg: &Config, move_dir: &str, path: &str) -> PathBuf {
    let root = cfg.root_for_path(path);
    let rel: String = match root {
        Some(r) => {
            let rp = crate::paths::canon(&r.path);
            let p = crate::paths::canon(path);
            p.get(rp.trim_end_matches(['\\', '/']).len()..).unwrap_or("").trim_start_matches(['\\', '/']).to_string()
        }
        None => Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
    };
    let safe = |s: &str| s.chars().map(|c| if r#"<>:"|?*"#.contains(c) { '_' } else { c }).collect::<String>();
    let mut base = PathBuf::from(move_dir).join(safe(root.map(|r| r.name.as_str()).unwrap_or("other")));
    for part in rel.split(['\\', '/']).filter(|p| !p.is_empty()) {
        base.push(part);
    }
    if !base.exists() {
        return base;
    }
    let (dir, fname) = (base.parent().map(|p| p.to_path_buf()).unwrap_or_default(), base.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
    let (stem, ext) = match fname.rfind('.') {
        Some(i) if i > 0 => (fname[..i].to_string(), fname[i..].to_string()),
        _ => (fname.clone(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Give the keeper what the removed copy carried (only what it lacks).
/// Returns true when anything was added.
fn merge_annotations(conn: &mut Connection, keeper: i64, loser: i64) -> bool {
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM change_log", [], |r| r.get(0)).unwrap_or(0);
    merge_annotations_inner(conn, keeper, loser);
    conn.query_row("SELECT COUNT(*) FROM change_log", [], |r| r.get::<_, i64>(0)).unwrap_or(0) > before
}

fn merge_annotations_inner(conn: &mut Connection, keeper: i64, loser: i64) {
    let k = crate::library::annotation(conn, keeper);
    let l = crate::library::annotation(conn, loser);
    let has_tag: HashSet<i64> = k["tags"].as_array().map(|a| a.iter().filter_map(|t| t["id"].as_i64()).collect()).unwrap_or_default();
    let names: Vec<String> = l["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|t| !t["id"].as_i64().map(|i| has_tag.contains(&i)).unwrap_or(true))
                .filter_map(|t| {
                    let n = t["name"].as_str()?;
                    Some(match t["category"].as_str() {
                        Some(c) => format!("{c}: {n}"),
                        None => n.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if !names.is_empty() {
        let _ = crate::library::add_tags(conn, &[keeper], &names, None);
    }
    let in_album: HashSet<i64> = k["albums"].as_array().map(|a| a.iter().filter_map(|x| x["id"].as_i64()).collect()).unwrap_or_default();
    for al in l["albums"].as_array().cloned().unwrap_or_default() {
        if let Some(id) = al["id"].as_i64() {
            if !in_album.contains(&id) {
                let _ = crate::library::add_to_album(conn, id, &[keeper]);
            }
        }
    }
    let ln = l["notes"].as_str().unwrap_or("").trim().to_string();
    if !ln.is_empty() && !k["notes"].as_str().unwrap_or("").contains(&ln) {
        let _ = crate::library::append_notes(conn, &[keeper], &ln);
    }
    let (kr, lr) = (k["rating"].as_i64().unwrap_or(0), l["rating"].as_i64().unwrap_or(0));
    if lr > kr {
        let _ = crate::library::set_field(conn, &[keeper], "rating", lr.into());
    }
    if l["favorite"].as_bool() == Some(true) && k["favorite"].as_bool() != Some(true) {
        let _ = crate::library::set_field(conn, &[keeper], "favorite", 1i64.into());
    }
    if k["colorLabel"].is_null() {
        if let Some(c) = l["colorLabel"].as_str() {
            let _ = crate::library::set_field(conn, &[keeper], "color_label", c.to_string().into());
        }
    }
    let ld = l["aiDescription"].as_str().unwrap_or("");
    if !ld.is_empty() && k["aiDescription"].as_str().unwrap_or("").is_empty() {
        let _ = crate::library::set_description(conn, keeper, ld);
    }
}

pub struct Plan {
    pub group: String,
    pub keep: i64,
    pub remove: Vec<i64>,
}

/// Remove the chosen copies. Each plan is checked against the groups as they
/// are NOW (files may have changed since the list was shown).
pub fn resolve(conn: &mut Connection, cfg: &Config, plans: Vec<Plan>, auto: bool) -> Result<Value, String> {
    let s = dup_settings(conn);
    if s.action == "move" && s.move_dir.trim().is_empty() {
        return Err("choose a folder for removed copies in Settings → Duplicates".into());
    }
    let current: HashMap<String, Value> =
        find_groups(conn, cfg, &s, true).into_iter().map(|g| (g["id"].as_str().unwrap_or("").to_string(), g)).collect();
    // (group, keeper copy, copies to remove)
    let mut work: Vec<(Value, Value, Vec<Value>)> = vec![];
    let mut skipped: Vec<String> = vec![];
    for p in plans {
        let Some(g) = current.get(&p.group) else {
            skipped.push(format!("{}: changed since it was listed — refresh and try again", p.group));
            continue;
        };
        let members = g["members"].as_array().cloned().unwrap_or_default();
        let Some(keep) = members.iter().find(|m| m["occId"].as_i64() == Some(p.keep)).cloned() else {
            skipped.push(format!("{}: the copy to keep is no longer part of this group", g["title"]));
            continue;
        };
        let rm: Vec<Value> = members
            .iter()
            .filter(|m| m["occId"].as_i64() != Some(p.keep) && p.remove.contains(&m["occId"].as_i64().unwrap_or(-1)))
            .filter(|m| m["path"] != keep["path"])
            .cloned()
            .collect();
        if rm.iter().any(|m| m["protected"].as_bool() == Some(true)) {
            skipped.push(format!("{}: copies in protected folders are kept", g["title"]));
        }
        let rm: Vec<Value> = rm.into_iter().filter(|m| m["protected"].as_bool() != Some(true)).collect();
        if !rm.is_empty() {
            work.push((g.clone(), keep, rm));
        }
    }
    if work.is_empty() {
        return Ok(json!({ "runId": null, "files": 0, "groups": 0, "bytes": 0, "failed": [], "skipped": skipped }));
    }

    // The Recycle Bin must really exist on every drive involved.
    if s.action == "recycle" {
        let mut checked: HashSet<String> = HashSet::new();
        for (_, _, rm) in &work {
            for m in rm {
                let root = cfg.root_for_path(m["path"].as_str().unwrap_or("")).ok_or("file outside the library folders")?;
                if checked.insert(root.path.clone()) {
                    crate::library::ensure_recycle_bin(&root.path)?;
                }
            }
        }
    }

    conn.execute("INSERT INTO dedupe_runs (at, action, auto) VALUES (?1, ?2, ?3)", params![now(), s.action, auto as i64])
        .map_err(|e| e.to_string())?;
    let run = conn.last_insert_rowid();
    let (mut files, mut bytes, mut groups_done) = (0i64, 0i64, 0i64);
    let mut failed: Vec<String> = vec![];
    let mut losers: Vec<(i64, i64)> = vec![]; // (keeper media, loser media)
    for (_, keep, rm) in &work {
        let keeper_media = keep["mediaId"].as_i64().unwrap_or(0);
        if !Path::new(keep["path"].as_str().unwrap_or("")).is_file() {
            failed.push(format!("{}: the copy to keep is not on disk — nothing removed", keep["path"].as_str().unwrap_or("")));
            continue;
        }
        let mut any = false;
        for m in rm {
            let path = m["path"].as_str().unwrap_or("").to_string();
            let (occ, media, b) = (m["occId"].as_i64().unwrap_or(0), m["mediaId"].as_i64().unwrap_or(0), m["bytes"].as_i64().unwrap_or(0));
            let p = Path::new(&path);
            let result: Result<Option<String>, String> = if !p.exists() {
                Err("already gone".into())
            } else if s.action == "move" {
                let t = review_target(cfg, &s.move_dir, &path);
                move_file(p, &t).map(|_| Some(t.to_string_lossy().to_string()))
            } else {
                match trash::delete(p) {
                    Ok(()) if !p.exists() => Ok(None),
                    Ok(()) => Err("still on disk after delete".into()),
                    Err(e) => Err(e.to_string()),
                }
            };
            match result {
                Ok(new_path) => {
                    let _ = conn.execute(
                        "INSERT INTO dedupe_log (run_id, media_id, occ_id, keeper_media, path, new_path, bytes, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'done')",
                        params![run, media, occ, keeper_media, path, new_path, b],
                    );
                    let _ = conn.execute("UPDATE occurrence SET missing = 1, is_canonical = 0, dup_group = NULL, indexed_at = ?2 WHERE id = ?1", params![occ, now()]);
                    files += 1;
                    bytes += b;
                    any = true;
                    if media != keeper_media {
                        losers.push((keeper_media, media));
                    }
                }
                Err(e) => {
                    let _ = conn.execute(
                        "INSERT INTO dedupe_log (run_id, media_id, occ_id, keeper_media, path, bytes, status, error)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'failed', ?7)",
                        params![run, media, occ, keeper_media, path, b, e],
                    );
                    failed.push(format!("{path}: {e}"));
                }
            }
        }
        if any {
            groups_done += 1;
        }
    }

    // Pictures with no copy left: merge what they carried into the keeper and
    // mark them deleted (Recycle Bin) so "Recently deleted" can restore them.
    losers.sort_unstable();
    losers.dedup();
    let mut touched = vec![];
    for (keeper, loser) in losers {
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM occurrence WHERE media_id = ?1 AND missing = 0", [loser], |r| r.get(0))
            .unwrap_or(1);
        if left > 0 {
            continue;
        }
        if s.merge_annotations {
            let src = crate::history::Source { dedupe: Some(run), ..Default::default() };
            if crate::history::with_source(src, || merge_annotations(conn, keeper, loser)) {
                touched.push(keeper);
            }
        }
        if s.action == "recycle" {
            let _ = conn.execute("UPDATE media SET deleted_at = ?2 WHERE id = ?1", params![loser, now()]);
        }
    }
    let _ = conn.execute(
        "UPDATE dedupe_runs SET groups = ?2, files = ?3, bytes = ?4, failed = ?5 WHERE id = ?1",
        params![run, groups_done, files, bytes, failed.len() as i64],
    );
    crate::library::recompute_dups(conn, cfg);
    crate::query::force_rebuild_fts(conn);
    let where_to = if s.action == "move" { format!("moved to {}", s.move_dir) } else { "sent to the Recycle Bin".into() };
    crate::library::log_activity(
        conn,
        if auto { "duplicates.auto" } else { "duplicates.clean" },
        files as usize,
        &format!("{files} duplicate file(s) {where_to}, {} freed (run {run}){}", fmt_bytes(bytes), if failed.is_empty() { String::new() } else { format!(", {} failed", failed.len()) }),
    );
    Ok(json!({
        "runId": run, "groups": groups_done, "files": files, "bytes": bytes,
        "action": s.action, "failed": failed, "skipped": skipped, "merged": touched.len(),
    }))
}

/// Resolve every group of the given kinds with the default choice of keeper.
pub fn resolve_all(conn: &mut Connection, cfg: &Config, kinds: &[&str], auto: bool) -> Result<Value, String> {
    fingerprint_pending(conn, &|_, _| {});
    let s = dup_settings(conn);
    let plans: Vec<Plan> = find_groups(conn, cfg, &s, false)
        .into_iter()
        .filter(|g| kinds.contains(&g["kind"].as_str().unwrap_or("")))
        .map(|g| Plan {
            group: g["id"].as_str().unwrap_or("").to_string(),
            keep: g["keepOccId"].as_i64().unwrap_or(0),
            remove: g["members"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|m| m["keep"] != json!(true) && m["protected"] != json!(true))
                        .filter_map(|m| m["occId"].as_i64())
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    resolve(conn, cfg, plans, auto)
}

/// After a sync: clean up automatically when Settings ask for it.
pub fn auto_resolve(conn: &mut Connection, cfg: &Config) -> Option<Value> {
    let s = dup_settings(conn);
    let kinds: &[&str] = match s.auto.as_str() {
        "exact" => &["exact"],
        "copies" => &["exact", "copies"],
        _ => return None,
    };
    match resolve_all(conn, cfg, kinds, true) {
        Ok(v) if v["files"].as_i64().unwrap_or(0) > 0 => Some(v),
        Ok(_) => None,
        Err(e) => Some(json!({ "error": e })),
    }
}

/// Put everything one clean-up removed back where it was.
pub fn undo_run(conn: &mut Connection, cfg: &Config, run: i64) -> Result<Value, String> {
    let (status, at): (String, i64) = conn
        .query_row("SELECT status, at FROM dedupe_runs WHERE id = ?1", [run], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|_| format!("clean-up {run} not found"))?;
    if status == "undone" {
        return Err("this clean-up was already undone".into());
    }
    let rows: Vec<(i64, String, Option<String>)> = conn
        .prepare("SELECT id, path, new_path FROM dedupe_log WHERE run_id = ?1 AND status = 'done' ORDER BY id DESC")
        .and_then(|mut st| st.query_map([run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect()))
        .map_err(|e| e.to_string())?;
    let mut back: Vec<(i64, PathBuf)> = vec![];
    let mut problems: Vec<String> = vec![];

    // Moved copies: move them back.
    for (id, path, new_path) in rows.iter().filter(|r| r.2.is_some()) {
        let (from, to) = (PathBuf::from(new_path.as_ref().unwrap()), PathBuf::from(path));
        if to.exists() {
            problems.push(format!("{path}: a file with that name exists again — left in {}", from.display()));
        } else if !from.exists() {
            problems.push(format!("{}: no longer in the review folder", from.display()));
        } else {
            match move_file(&from, &to) {
                Ok(()) => back.push((*id, to)),
                Err(e) => problems.push(format!("{path}: {e}")),
            }
        }
    }
    // Recycled copies: restore from the bin (newest entry per original path,
    // deleted no earlier than the clean-up started).
    let recycled: HashMap<String, (i64, String)> =
        rows.iter().filter(|r| r.2.is_none()).map(|(id, p, _)| (crate::paths::key(p), (*id, p.clone()))).collect();
    if !recycled.is_empty() {
        let bin = trash::os_limited::list().map_err(|e| format!("cannot read the Recycle Bin: {e}"))?;
        let mut pick: HashMap<String, trash::TrashItem> = HashMap::new();
        for it in bin {
            let k = crate::paths::key(&it.original_path().to_string_lossy());
            if !recycled.contains_key(&k) || it.time_deleted < at - 120 {
                continue;
            }
            if pick.get(&k).map(|o| it.time_deleted > o.time_deleted).unwrap_or(true) {
                pick.insert(k, it);
            }
        }
        let mut restore = vec![];
        for (k, (id, p)) in &recycled {
            match pick.remove(k) {
                Some(it) if it.original_path().exists() => problems.push(format!("{p}: a file with that name exists again — not overwriting")),
                Some(it) => {
                    back.push((*id, it.original_path()));
                    restore.push(it);
                }
                None => problems.push(format!("{p}: no longer in the Recycle Bin")),
            }
        }
        if !restore.is_empty() {
            trash::os_limited::restore_all(restore).map_err(|e| format!("restore failed: {e}"))?;
        }
    }
    let mut restored = 0;
    for (id, p) in &back {
        if p.exists() && crate::incremental::process_file(conn, cfg, p).is_ok() {
            restored += 1;
            let _ = conn.execute("UPDATE dedupe_log SET status = 'undone' WHERE id = ?1", [id]);
        }
    }
    // Merged notes / tags / ratings go back too.
    let merged = crate::history::undo_job(conn, -run);
    let _ = conn.execute("UPDATE dedupe_runs SET status = 'undone', undone_at = ?2 WHERE id = ?1", params![run, now()]);
    crate::library::recompute_dups(conn, cfg);
    crate::query::force_rebuild_fts(conn);
    crate::library::log_activity(conn, "duplicates.undo", restored, &format!("clean-up {run} undone: {restored} file(s) back"));
    Ok(json!({ "restored": restored, "problems": problems, "annotationsUndone": merged["undone"] }))
}

fn runs(conn: &Connection, limit: i64) -> Value {
    let rows: Vec<Value> = conn
        .prepare("SELECT id, at, action, auto, groups, files, bytes, failed, status FROM dedupe_runs ORDER BY id DESC LIMIT ?1")
        .and_then(|mut st| {
            st.query_map([limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "at": r.get::<_, i64>(1)?, "action": r.get::<_, String>(2)?,
                    "auto": r.get::<_, i64>(3)? == 1, "groups": r.get::<_, i64>(4)?, "files": r.get::<_, i64>(5)?,
                    "bytes": r.get::<_, i64>(6)?, "failed": r.get::<_, i64>(7)?, "status": r.get::<_, String>(8)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

fn plans_from(a: &Value) -> Result<Vec<Plan>, String> {
    let list = a.get("groups").and_then(|v| v.as_array()).ok_or("missing groups")?;
    list.iter()
        .map(|g| {
            Ok(Plan {
                group: g.get("id").and_then(|v| v.as_str()).ok_or("group without id")?.to_string(),
                keep: g.get("keepOccId").and_then(|v| v.as_i64()).ok_or("group without keepOccId")?,
                remove: g
                    .get("removeOccIds")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

pub fn call(conn: &mut Connection, cfg: &Config, cmd: &str, a: &Value) -> Result<Value, String> {
    match cmd {
        "dups.scan" => Ok(scan(conn, cfg, a.get("includeIgnored").and_then(|v| v.as_bool()).unwrap_or(false))),
        "dups.resolve" => resolve(conn, cfg, plans_from(a)?, false),
        "dups.resolveAll" => {
            let kinds: Vec<String> = a
                .get("kinds")
                .and_then(|v| v.as_array())
                .map(|k| k.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec!["exact".into(), "copies".into()]);
            let k: Vec<&str> = kinds.iter().map(|s| s.as_str()).filter(|k| *k != "similar").collect();
            if k.is_empty() {
                return Err("look-alike photos are only removed one group at a time, after review".into());
            }
            resolve_all(conn, cfg, &k, false)
        }
        "dups.undo" => undo_run(conn, cfg, a.get("runId").and_then(|v| v.as_i64()).ok_or("missing runId")?),
        "dups.runs" => Ok(runs(conn, a.get("limit").and_then(|v| v.as_i64()).unwrap_or(20))),
        "dups.ignore" => {
            let id = a.get("id").and_then(|v| v.as_str()).ok_or("missing id")?;
            conn.execute("INSERT OR REPLACE INTO dup_ignore (sig, at) VALUES (?1, ?2)", params![id, now()]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "dups.unignoreAll" => {
            let n = conn.execute("DELETE FROM dup_ignore", []).map_err(|e| e.to_string())?;
            Ok(json!({ "restored": n }))
        }
        _ => Err(format!("unknown command: {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(occ: i64, media: i64, root: &str, prio: usize, ext: &str, w: i64, h: i64, bytes: i64, cam: bool, name: &str) -> Copy {
        Copy {
            occ_id: occ, media_id: media, path: format!("/{root}/{name}"), root: root.into(), kind: "image".into(),
            ext: ext.into(), bytes, w: Some(w), h: Some(h), taken_at: Some("2020-01-01 10:00:00".into()),
            date_source: Some(if cam { "exif" } else { "mtime" }.into()), camera: cam.then(|| "Canon".to_string()),
            thumb: None, phash: Some(0), priority: prio, protected: false,
        }
    }

    #[test]
    fn copy_names() {
        assert!(looks_like_a_copy("IMG_1234 (2).jpg"));
        assert!(looks_like_a_copy("Copy of IMG_1234.jpg"));
        assert!(looks_like_a_copy("IMG_1234 - Copy.JPG"));
        assert!(!looks_like_a_copy("IMG_1234.jpg"));
        assert!(!looks_like_a_copy("Trip (Maine).jpg"));
        assert!(!looks_like_a_copy("2013-06-13_164706_2.jpg"));
    }

    #[test]
    fn keeper_is_highest_quality_then_folder() {
        let s = DupSettings::default();
        let big = c(1, 1, "Backup", 1, "jpg", 4000, 3000, 3_000_000, true, "a.jpg");
        let small = c(2, 2, "Main", 0, "jpg", 1600, 1200, 400_000, false, "a.jpg");
        assert_eq!(keep_cmp(&big, &small, &s), Ordering::Greater);
        assert_eq!(relation(&big, &small), "lower");
        // Folder rule: the main folder wins even with the smaller copy.
        let f = DupSettings { keep_rule: "folder".into(), ..DupSettings::default() };
        assert_eq!(keep_cmp(&small, &big, &f), Ordering::Greater);
        // Identical bytes: original name, then folder priority.
        let a = c(3, 5, "Backup", 1, "jpg", 10, 10, 9, true, "x.jpg");
        let b = c(4, 5, "Main", 0, "jpg", 10, 10, 9, true, "x (1).jpg");
        assert_eq!(keep_cmp(&a, &b, &s), Ordering::Greater);
        let b2 = c(4, 5, "Main", 0, "jpg", 10, 10, 9, true, "x.jpg");
        assert_eq!(keep_cmp(&b2, &a, &s), Ordering::Greater);
    }

    #[test]
    fn keeper_sort_is_a_total_order() {
        // Sizes 0.6 % apart: each pair is "about equal", the ends are not.
        // The old 1 %-tolerance comparison made sort_by panic on this.
        let s = DupSettings::default();
        let mut v: Vec<Copy> = (0..60)
            .map(|i| {
                let w = 1000 + i * 3;
                let mut x = c(i, i, if i % 3 == 0 { "A" } else { "B" }, (i % 3) as usize, if i % 2 == 0 { "jpg" } else { "png" }, w, 1000, 1000 + (i * 7919) % 500, i % 4 == 0, "x.jpg");
                x.path = format!("/p/{}/x.jpg", "d".repeat((i % 5) as usize));
                x
            })
            .collect();
        for _ in 0..20 {
            v.reverse();
            v.sort_by(|a, b| keep_cmp(b, a, &s));
            v.rotate_left(7);
            v.sort_by(|a, b| keep_cmp(b, a, &DupSettings { keep_rule: "folder".into(), ..DupSettings::default() }));
        }
    }

    #[test]
    fn lookalikes_are_not_lower() {
        let k = c(1, 1, "M", 0, "jpg", 4000, 3000, 3_000_000, true, "a.jpg");
        let burst = c(2, 2, "M", 0, "jpg", 4000, 3000, 2_900_000, true, "b.jpg");
        assert_eq!(relation(&k, &burst), "lookalike");
        // A PNG converted from the camera JPEG loses to it (camera data first).
        let png = c(3, 3, "M", 0, "png", 4000, 3000, 9_000_000, false, "a.png");
        assert_eq!(keep_cmp(&k, &png, &DupSettings::default()), Ordering::Greater);
        assert_eq!(relation(&k, &png), "lower");
        // Two copies with camera data: lossless TIFF edit beats the JPEG.
        let tif = c(5, 5, "M", 0, "tif", 4000, 3000, 30_000_000, true, "a.tif");
        assert_eq!(keep_cmp(&tif, &k, &DupSettings::default()), Ordering::Greater);
        assert_eq!(relation(&tif, &k), "lower");
        let stripped = c(4, 4, "M", 0, "jpg", 4000, 3000, 1_000_000, false, "a.jpg");
        assert_eq!(relation(&k, &stripped), "lower");
    }

    #[test]
    fn similar_needs_close_hash_shape_and_date() {
        let s = DupSettings::default();
        let mut a = c(1, 1, "M", 0, "jpg", 4000, 3000, 1, true, "a.jpg");
        let mut b = c(2, 2, "M", 0, "jpg", 2000, 1500, 1, true, "b.jpg");
        a.phash = Some(0b1011);
        b.phash = Some(0b1000);
        assert_eq!(similar_pair(&a, &b, &s), Some(2));
        b.h = Some(1000); // different shape
        assert_eq!(similar_pair(&a, &b, &s), None);
        b.h = Some(1500);
        b.taken_at = Some("2021-05-05 10:00:00".into());
        assert_eq!(similar_pair(&a, &b, &s), None);
        b.date_source = Some("mtime".into()); // file time is not trusted for this
        assert_eq!(similar_pair(&a, &b, &s), Some(2));
        b.phash = Some(!0);
        assert_eq!(similar_pair(&a, &b, &s), None);
    }

    #[test]
    fn dhash_survives_resize_and_recompress() {
        let mut img = image::RgbImage::new(320, 240);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x % 256) as u8, (y * 255 / 240) as u8, ((x * y) % 255) as u8]);
        }
        let big = image::DynamicImage::ImageRgb8(img);
        let small = big.resize(80, 60, image::imageops::FilterType::Lanczos3);
        let mut buf = std::io::Cursor::new(vec![]);
        small.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        let jpg = image::load_from_memory(buf.get_ref()).unwrap();
        let d = (dhash(&big) ^ dhash(&jpg)).count_ones();
        assert!(d <= 6, "distance {d}");
        let other = big.rotate90();
        assert!((dhash(&big) ^ dhash(&other)).count_ones() > 10);
    }
}
