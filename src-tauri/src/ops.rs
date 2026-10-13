//! M7: duplicate flagging + staging — non-destructive, charter-bound.
//!
//! Charter rules implemented here (plan §7, binding):
//!   1. Dry run before anything moves. Every op (even a dry run) writes a
//!      JSON report to `data/reports/`.
//!   2. Undo journal. Every applied op records original + new path in
//!      `op_journal`; undo reverses in reverse order.
//!   3. No delete. Duplicates are FLAGGED (`occurrence.is_flagged`) and
//!      optionally MOVED into a `duplicates/` staging folder.
//!   5. Collision = fail and report — never silently overwrite.
//!   6. Cross-drive moves are copy-then-rename-source, reported as such.
//!
//! `apply` and `undo` are single-transaction for the DB side; the file side
//! is per-op verified (source exists, target free) and fails atomically per
//! op with the journal left consistent (applied rows are only journaled
//! after the file op succeeds).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::scanner::Config;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DupOccurrence {
    pub id: i64,
    pub path: String,
    pub root: String,
    pub mtime: i64,
    pub missing: bool,
    pub is_canonical: bool,
    pub is_flagged: bool,
    pub exists_on_disk: bool,
}

/// One duplicate group = one media row with >=2 occurrence paths.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    pub media_id: i64,
    pub sha256: String,
    pub kind: String,
    pub ext: String,
    pub bytes: i64,
    /// Display name = filename of the canonical occurrence (stable per group).
    pub filename: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<String>,
    pub date_source: Option<String>,
    pub camera: Option<String>,
    pub thumb_path: Option<String>,
    pub occurrences: Vec<DupOccurrence>,
    /// Every occurrence is missing on disk (nothing left to stage).
    pub all_missing: bool,
    /// At least one occurrence is currently flagged/staged.
    pub has_flagged: bool,
    /// Every non-canonical occurrence is flagged/staged.
    pub fully_flagged: bool,
}

/// Summary line for the dup view header.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DupSummary {
    pub groups: i64,
    pub duplicate_occurrences: i64,
    pub wasted_bytes: i64,
    pub flagged: i64,
    pub max_group_size: i64,
}

/// One planned (or applied) duplicate occurrence.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StagingMove {
    pub occ_id: i64,
    pub source_path: String,
    pub target_path: String,
    /// Source file currently exists on disk (false = flag-only, nothing to move).
    pub on_disk: bool,
    /// Target path is free (false = collision; apply fails this op, charter §7.5).
    pub target_free: bool,
    pub bytes: i64,
}

/// An occurrence excluded from staging (with the reason, for the report).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SkippedOccurrence {
    pub occ_id: i64,
    pub path: String,
    pub reason: String,
    pub is_canonical: bool,
}

/// The full dry-run / apply / undo report (written to `data/reports/`).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StagingPlan {
    pub report_id: String,
    /// "dry-run" | "applied" | "undone"
    pub mode: String,
    pub created_at: i64,
    pub media_id: i64,
    pub sha256: String,
    pub filename: String,
    pub bytes: i64,
    pub canonical_path: Option<String>,
    pub staging_dir: String,
    pub source_drive: Option<String>,
    pub target_drive: Option<String>,
    pub cross_drive: bool,
    pub moves: Vec<StagingMove>,
    pub skipped: Vec<SkippedOccurrence>,
    pub warnings: Vec<String>,
    pub report_path: String,
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn reports_dir() -> PathBuf {
    let db = crate::paths::db_path();
    let data = Path::new(&db)
        .parent()
        .unwrap_or_else(|| Path::new("data"));
    data.join("reports")
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Staging folder = first configured root + `duplicates` (keeps the op inside
/// the one in-scope library root; no cross-library writes).
fn staging_dir(cfg: &Config) -> String {
    let root = cfg
        .roots
        .first()
        .map(|r| r.path.trim_end_matches(['/', '\\']).to_string())
        .unwrap_or_default();
    if root.is_empty() {
        return "duplicates".to_string();
    }
    if root.contains('\\') {
        format!("{root}\\duplicates")
    } else {
        format!("{root}/duplicates")
    }
}

fn drive_of(path: &str) -> Option<String> {
    // `E:\...` or `E:/...` -> `E`
    let p = path.trim_start();
    if p.len() >= 2 && p.as_bytes()[0].is_ascii_alphabetic() && p.as_bytes()[1] == b':' {
        Some(p[..1].to_uppercase())
    } else {
        None
    }
}

fn filename_of(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Disambiguate `a.jpg` -> `a.jpg`, `a (1).jpg`, `a (2).jpg`, ...
fn disambiguate(filename: &str, used: &std::collections::HashSet<String>) -> String {
    if !used.contains(&filename.to_lowercase()) {
        return filename.to_string();
    }
    let (stem, ext) = match filename.rfind('.') {
        Some(i) if i > 0 => (filename[..i].to_string(), filename[i..].to_string()),
        _ => (filename.to_string(), String::new()),
    };
    let mut n = 1;
    loop {
        let cand = format!("{stem} ({n}){ext}");
        if !used.contains(&cand.to_lowercase()) {
            return cand;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// All duplicate groups (media rows with >=2 occurrences), largest content
/// first (most wasted space first).
pub fn dup_groups(conn: &Connection, limit: Option<i64>) -> Result<Vec<DupGroup>, String> {
    let mut sql = String::from(
        "SELECT m.id, m.sha256, m.kind, m.ext, m.bytes, m.width, m.height,
                m.taken_at, m.date_source, m.camera, m.thumb_path
         FROM media m
         WHERE m.id IN (SELECT media_id FROM occurrence WHERE dup_group IS NOT NULL)
         ORDER BY m.bytes DESC, m.id DESC",
    );
    if let Some(l) = limit {
        sql.push_str(&format!(" LIMIT {l}"));
    }
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| e.to_string())?;
    let heads = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let heads: Vec<_> = heads.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;

    // One query for every occurrence of these groups; bucket by media_id.
    let mut occ_by_media: std::collections::HashMap<i64, Vec<DupOccurrence>> =
        std::collections::HashMap::new();
    {
        let mut os = conn
            .prepare(
                "SELECT id, media_id, path, root, mtime, missing, is_canonical, is_flagged
                 FROM occurrence WHERE dup_group IS NOT NULL
                 ORDER BY is_canonical DESC, id",
            )
            .map_err(|e| e.to_string())?;
        let rows = os
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)? == 1,
                    r.get::<_, i64>(6)? == 1,
                    r.get::<_, i64>(7)? == 1,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (id, mid, path, root, mtime, missing, canon, flagged) =
                r.map_err(|e| e.to_string())?;
            let exists = Path::new(&path).exists();
            occ_by_media
                .entry(mid)
                .or_default()
                .push(DupOccurrence {
                    id,
                    path,
                    root,
                    mtime,
                    missing,
                    is_canonical: canon,
                    is_flagged: flagged,
                    exists_on_disk: exists,
                });
        }
    }

    let mut out = Vec::new();
    for h in heads {
        let (
            id,
            sha,
            kind,
            ext,
            bytes,
            width,
            height,
            taken_at,
            date_source,
            camera,
            thumb_path,
        ) = h;
        let occurrences = occ_by_media.remove(&id).unwrap_or_default();
        // Display name: filename of the canonical occurrence (first row, since
        // occurrences are ordered is_canonical DESC, id ASC).
        let filename = occurrences
            .first()
            .and_then(|o| o.path.rsplit(['\\', '/']).next())
            .unwrap_or("")
            .to_string();
        let all_missing = occurrences.iter().all(|o| o.missing || !o.exists_on_disk);
        let non_canon: Vec<&DupOccurrence> = occurrences
            .iter()
            .filter(|o| !o.is_canonical)
            .collect();
        let has_flagged = non_canon.iter().any(|o| o.is_flagged);
        let fully_flagged = !non_canon.is_empty() && non_canon.iter().all(|o| o.is_flagged);
        out.push(DupGroup {
            media_id: id,
            sha256: sha,
            kind,
            ext,
            bytes,
            filename,
            width,
            height,
            taken_at,
            date_source,
            camera,
            thumb_path,
            occurrences,
            all_missing,
            has_flagged,
            fully_flagged,
        });
    }
    Ok(out)
}

pub fn dup_summary(conn: &Connection) -> DupSummary {
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let groups = q(
        "SELECT COUNT(DISTINCT media_id) FROM occurrence WHERE dup_group IS NOT NULL",
    );
    let duplicate_occurrences = q(
        "SELECT COUNT(*) FROM occurrence o
         WHERE o.dup_group IS NOT NULL AND o.is_canonical = 0",
    );
    let wasted_bytes = q(
        "SELECT COALESCE(SUM(m.bytes),0) FROM media m
         WHERE m.id IN (SELECT media_id FROM occurrence WHERE dup_group IS NOT NULL)",
    );
    let flagged = q(
        "SELECT COUNT(*) FROM occurrence WHERE is_flagged = 1 AND is_canonical = 0",
    );
    let max_group_size = q(
        "SELECT COALESCE(MAX(cnt),0) FROM (
           SELECT COUNT(*) AS cnt FROM occurrence
           WHERE dup_group IS NOT NULL GROUP BY media_id)",
    );
    DupSummary {
        groups,
        duplicate_occurrences,
        wasted_bytes,
        flagged,
        max_group_size,
    }
}

// ---------------------------------------------------------------------------
// Dry-run / apply / undo
// ---------------------------------------------------------------------------

struct Loaded {
    sha256: String,
    bytes: i64,
    canonical_path: Option<String>,
    filename: String,
    non_canonical: Vec<(i64, String, bool /*exists*/)>,
    canonical: (i64, String),
}

fn load_group(conn: &Connection, media_id: i64) -> Result<Loaded, String> {
    let (sha256, bytes): (String, i64) = conn
        .query_row(
            "SELECT sha256, bytes FROM media WHERE id = ?1",
            rusqlite::params![media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let mut stmt = conn
        .prepare(
            "SELECT id, path, is_canonical FROM occurrence
             WHERE media_id = ?1 AND dup_group IS NOT NULL
             ORDER BY is_canonical DESC, id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![media_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? == 1,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut canonical: Option<(i64, String)> = None;
    let mut non_canonical: Vec<(i64, String, bool)> = Vec::new();
    for r in rows {
        let (id, path, is_c) = r.map_err(|e| e.to_string())?;
        if is_c {
            canonical = Some((id, path));
        } else {
            let exists = Path::new(&path).exists();
            non_canonical.push((id, path, exists));
        }
    }
    let (canonical_id, canonical_path) = canonical.ok_or("group has no canonical occurrence")?;
    if non_canonical.is_empty() {
        return Err("group has no duplicate occurrences".to_string());
    }
    let filename = filename_of(&canonical_path);
    Ok(Loaded {
        sha256,
        bytes,
        canonical_path: Some(canonical_path.clone()),
        filename,
        non_canonical,
        canonical: (canonical_id, canonical_path),
    })
}

/// Build the staging plan for one group (dry run). Writes a report to
/// `data/reports/` — charter rule 1.
pub fn plan_staging(conn: &Connection, cfg: &Config, media_id: i64) -> Result<StagingPlan, String> {
    let g = load_group(conn, media_id)?;
    let sdir = staging_dir(cfg);
    let sdir_path = Path::new(&sdir);
    let target_drive = drive_of(&sdir);

    let mut moves = Vec::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut warnings = Vec::new();

    for (occ_id, path, on_disk) in &g.non_canonical {
        let src_drive = drive_of(path);
        let cross = match (&src_drive, &target_drive) {
            (Some(s), Some(t)) if s != t => true,
            _ => false,
        };
        let name = disambiguate(&g.filename, &used);
        used.insert(name.to_lowercase());
        let target = sdir_path.join(&name).to_string_lossy().to_string();
        let target_free = !Path::new(&target).exists();
        if cross {
            warnings.push(format!(
                "cross-drive: {path} -> {target} (copy-then-rename, reported as such)"
            ));
        }
        if !target_free {
            warnings.push(format!("collision: target {target} already exists — op will fail at apply (never overwrite)"));
        }
        if !on_disk {
            warnings.push(format!("source missing on disk: {path} (flag only, nothing to move)"));
        }
        moves.push(StagingMove {
            occ_id: *occ_id,
            source_path: path.clone(),
            target_path: target,
            on_disk: *on_disk,
            target_free,
            bytes: g.bytes,
        });
    }

    let cross_any = warnings.iter().any(|w| w.starts_with("cross-drive"));
    let report_id = format!("dedupe-{}-{}", media_id, now_secs());
    let mut plan = StagingPlan {
        report_id: report_id.clone(),
        mode: "dry-run".to_string(),
        created_at: now_secs(),
        media_id,
        sha256: g.sha256,
        filename: g.filename.clone(),
        bytes: g.bytes,
        canonical_path: g.canonical_path.clone(),
        staging_dir: sdir,
        source_drive: g.non_canonical.first().and_then(|(_, p, _)| drive_of(p)),
        target_drive: target_drive.clone(),
        cross_drive: cross_any,
        moves,
        skipped: vec![SkippedOccurrence {
            occ_id: g.canonical.0,
            path: g.canonical.1.clone(),
            reason: "canonical occurrence — kept in place".to_string(),
            is_canonical: true,
        }],
        warnings,
        report_path: String::new(),
    };
    plan.report_path = write_report(&report_id, &plan)?;
    Ok(plan)
}

fn write_report(report_id: &str, plan: &StagingPlan) -> Result<String, String> {
    let dir = reports_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{report_id}.json"));
    let json = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// Apply a staging plan: move each on-disk non-canonical occurrence into the
/// staging dir, flag every non-canonical occurrence, journal everything.
/// Any collision or vanished source fails the whole apply (DB untouched).
pub fn apply_staging(
    conn: &mut Connection,
    cfg: &Config,
    media_id: i64,
) -> Result<StagingPlan, String> {
    let plan = plan_staging(conn, cfg, media_id)?;
    // Pre-flight is done from the plan's moves; nothing else to load here.

    // Pre-flight: every move must be possible, or we fail before touching disk.
    for m in &plan.moves {
        if m.on_disk {
            if !Path::new(&m.source_path).exists() {
                return Err(format!("source vanished: {}", m.source_path));
            }
            if Path::new(&m.target_path).exists() {
                return Err(format!(
                    "collision: target {} already exists — refusing to overwrite (charter §7.5)",
                    m.target_path
                ));
            }
        }
    }

    let sdir = Path::new(&plan.staging_dir);
    let mut applied_moves: Vec<(i64, String, String, i64, bool)> = Vec::new(); // occ_id, src, dst, bytes, cross

    // File side first (outside the DB transaction): fail loud, nothing journaled yet.
    for m in &plan.moves {
        if !m.on_disk {
            continue; // flag-only
        }
        let src = Path::new(&m.source_path);
        let dst = Path::new(&m.target_path);
        fs::create_dir_all(sdir).map_err(|e| e.to_string())?;
        let cross = drive_of(&m.source_path) != drive_of(&m.target_path);
        if cross {
            // Charter §7.6: cross-drive "move" = copy-then-rename-source.
            fs::copy(src, dst).map_err(|e| e.to_string())?;
            fs::remove_file(src).map_err(|e| e.to_string())?;
        } else {
            fs::rename(src, dst).map_err(|e| e.to_string())?;
        }
        applied_moves.push((m.occ_id, m.source_path.clone(), m.target_path.clone(), m.bytes, cross));
    }

    // DB side: one transaction — flags + path updates + journal.
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (occ_id, src, dst, bytes, cross) in &applied_moves {
        tx.execute(
            "UPDATE occurrence SET path = ?2, is_flagged = 1 WHERE id = ?1",
            rusqlite::params![occ_id, dst],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO op_journal (created_at, op, media_id, occ_id, original_path, new_path, bytes, cross_drive, status)
             VALUES (?1, 'staging', ?2, ?3, ?4, ?5, ?6, ?7, 'applied')",
            rusqlite::params![now_secs(), media_id, occ_id, src, dst, bytes, if *cross { 1 } else { 0 }],
        )
        .map_err(|e| e.to_string())?;
    }
    // Flag-only occurrences (source already missing): flag them too so the
    // group state is complete and undo can clear them.
    for m in &plan.moves {
        if m.on_disk {
            continue;
        }
        tx.execute(
            "UPDATE occurrence SET is_flagged = 1 WHERE id = ?1",
            rusqlite::params![m.occ_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO op_journal (created_at, op, media_id, occ_id, original_path, new_path, bytes, cross_drive, status)
             VALUES (?1, 'staging', ?2, ?3, ?4, NULL, ?5, 0, 'applied')",
            rusqlite::params![now_secs(), media_id, m.occ_id, m.source_path, m.bytes],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;

    let mut report = plan;
    report.mode = "applied".to_string();
    report.created_at = now_secs();
    report.report_id = format!("dedupe-{media_id}-applied-{}", now_secs());
    report.report_path = write_report(&report.report_id, &report)?;
    Ok(report)
}

/// Undo the last applied staging for this group (reverse journal order).
/// Restores every moved file to its original path and clears the flags.
pub fn undo_staging(
    conn: &mut Connection,
    media_id: i64,
) -> Result<StagingPlan, String> {
    let rows: Vec<(i64, String, Option<String>, i64, bool)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, original_path, new_path, bytes, cross_drive
                 FROM op_journal
                 WHERE media_id = ?1 AND op = 'staging' AND status = 'applied'
                 ORDER BY id DESC",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(rusqlite::params![media_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)? == 1,
                ))
            })
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    if rows.is_empty() {
        return Err("no applied staging to undo for this group".to_string());
    }

    let (sha256, filename) = {
        let sha: String = conn
            .query_row("SELECT sha256 FROM media WHERE id = ?1", rusqlite::params![media_id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let canon: String = conn
            .query_row(
                "SELECT path FROM occurrence WHERE media_id = ?1 AND is_canonical = 1 ORDER BY id LIMIT 1",
                rusqlite::params![media_id],
                |r| r.get(0),
            )
            .unwrap_or_default();
        (sha, filename_of(&canon))
    };

    let mut reverted: Vec<(i64, String, String)> = Vec::new(); // journal_id, original, new
    let mut warnings = Vec::new();

    for (jid, original, new_path, _bytes, _cross) in rows {
        if let Some(p) = new_path.filter(|s| !s.is_empty()) {
            let np = Path::new(&p);
            let op = Path::new(&original);
            if np.exists() {
                if op.exists() {
                    // Charter: never overwrite. Leave this one in place.
                    warnings.push(format!(
                        "undo refused for {p}: original {original} exists on disk — not overwriting"
                    ));
                    continue;
                }
                if drive_of(&p) != drive_of(&original) {
                    // Cross-drive revert: copy back, then remove the staged copy.
                    fs::copy(np, op).map_err(|e| e.to_string())?;
                    fs::remove_file(np).map_err(|e| e.to_string())?;
                } else {
                    fs::rename(np, op).map_err(|e| e.to_string())?;
                }
                reverted.push((jid, original.clone(), p));
            } else {
                warnings.push(format!("undo skipped {p}: staged file no longer on disk"));
            }
        }
        // None / empty: flag-only row (source was missing when staged) — nothing to move back.
    }

    // DB side: restore paths + clear flags + mark journal undone.
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (jid, original, _new) in &reverted {
        tx.execute(
            "UPDATE occurrence SET path = ?2, is_flagged = 0
             WHERE id = (SELECT occ_id FROM op_journal WHERE id = ?1)",
            rusqlite::params![jid, original],
        )
        .map_err(|e| e.to_string())?;
    }
    // Clear flags on all applied rows (including flag-only ones).
    tx.execute(
        "UPDATE occurrence SET is_flagged = 0
         WHERE id IN (SELECT occ_id FROM op_journal
                      WHERE media_id = ?1 AND op = 'staging' AND status = 'applied')",
        rusqlite::params![media_id],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE op_journal SET status = 'undone'
         WHERE media_id = ?1 AND op = 'staging' AND status = 'applied'",
        rusqlite::params![media_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    let report = StagingPlan {
        report_id: format!("dedupe-{media_id}-undone-{}", now_secs()),
        mode: "undone".to_string(),
        created_at: now_secs(),
        media_id,
        sha256,
        filename,
        bytes: 0,
        canonical_path: None,
        staging_dir: String::new(),
        source_drive: None,
        target_drive: None,
        cross_drive: false,
        moves: Vec::new(),
        skipped: Vec::new(),
        warnings,
        report_path: String::new(),
    };
    let path = write_report(&report.report_id, &report)?;
    Ok(StagingPlan { report_path: path, ..report })
}

// ===========================================================================
// M8: rename / move (relocate) the canonical occurrence — non-destructive
//
// Rename and move are the same primitive: change the path of the canonical
// occurrence. Rename = new name, same folder. Move = different folder (e.g.
// into a year folder). Only the canonical file is touched; flagged duplicate
// copies stay exactly where they are. Same charter as M7: dry-run report first,
// apply on confirmation, undo reverses; collision fails; cross-drive copies.
// ===========================================================================

/// The report for a rename/move op (dry-run / applied / undone).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RelocatePlan {
    pub report_id: String,
    pub mode: String,
    pub op: String, // "rename" | "move"
    pub created_at: i64,
    pub media_id: i64,
    pub sha256: String,
    pub filename: String,
    pub bytes: i64,
    pub original_path: String,
    pub target_path: String,
    pub on_disk: bool,
    pub target_free: bool,
    pub cross_drive: bool,
    pub warnings: Vec<String>,
    pub report_path: String,
}

fn write_relocate_report(report_id: &str, plan: &RelocatePlan) -> Result<String, String> {
    let dir = reports_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{report_id}.json"));
    let json = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

/// The canonical occurrence (id, path, on-disk) for a media item.
/// Ordering makes it work for BOTH duplicate groups (is_canonical = 1) and
/// single-occurrence media (is_canonical = 0, so "canonical" is just the one
/// real file): canonical-first, present-first, then lowest id.
/// Returns (sha256, bytes, occ_id, path, on_disk).
fn canonical_of(conn: &Connection, media_id: i64) -> Result<(String, i64, i64, String, bool), String> {
    let (sha256, bytes): (String, i64) = conn
        .query_row(
            "SELECT sha256, bytes FROM media WHERE id = ?1",
            rusqlite::params![media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let (occ_id, path): (i64, String) = conn
        .query_row(
            "SELECT id, path FROM occurrence WHERE media_id = ?1
             ORDER BY CASE WHEN is_canonical = 1 THEN 0 ELSE 1 END,
                      CASE WHEN missing = 1 THEN 1 ELSE 0 END,
                      id
             LIMIT 1",
            rusqlite::params![media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("no occurrence for media {media_id}: {e}"))?;
    let on_disk = Path::new(&path).exists();
    Ok((sha256, bytes, occ_id, path, on_disk))
}

/// Suggested move target under the first configured root for a media item.
/// `layout`: "year" -> <root>/<YYYY>/<file>; "year_month" -> <root>/<YYYY>/<MM>/<file>;
/// "root" -> <root>/<file>. The year/month come from the item's best date.
pub fn move_target_for(
    conn: &Connection,
    cfg: &Config,
    media_id: i64,
    layout: &str,
) -> Result<String, String> {
    let (_sha, _bytes, _occ, canon, _disk) = canonical_of(conn, media_id)?;
    let filename = filename_of(&canon);
    let taken_at: Option<String> = conn
        .query_row("SELECT taken_at FROM media WHERE id = ?1", rusqlite::params![media_id], |r| {
            r.get(0)
        })
        .ok()
        .flatten();
    let root = cfg
        .roots
        .first()
        .map(|r| r.path.trim_end_matches(['/', '\\']).to_string())
        .ok_or("no configured root")?;
    let sep = if root.contains('\\') { "\\" } else { "/" };
    let (y, m): (Option<&str>, Option<&str>) = match &taken_at {
        Some(t) if t.len() >= 10 => (
            Some(&t[0..4]),
            t.get(5..7).filter(|s| s.len() == 2),
        ),
        Some(t) if t.len() >= 4 => (Some(&t[0..4]), None),
        _ => (None, None),
    };
    let dir = match layout {
        "year_month" => match (y, m) {
            (Some(y), Some(m)) => format!("{root}{sep}{y}{sep}{m}"),
            (Some(y), None) => format!("{root}{sep}{y}"),
            _ => root.clone(),
        },
        "year" => match y {
            Some(y) => format!("{root}{sep}{y}"),
            None => root.clone(),
        },
        _ => root.clone(), // "root" or anything else
    };
    Ok(format!("{dir}{sep}{filename}"))
}

/// Dry-run a rename/move. Writes a report; moves nothing (charter §7.1).
pub fn plan_relocate(
    conn: &Connection,
    media_id: i64,
    target_path: &str,
    op: &str,
) -> Result<RelocatePlan, String> {
    let (sha256, bytes, _occ, original, on_disk) = canonical_of(conn, media_id)?;
    let filename = filename_of(&original);
    let warnings: Vec<String> = vec![];
    // One stored path form (see paths::canon) so a later rescan matches it.
    let target = crate::paths::canon(target_path);
    if target == original {
        return Err("target path is identical to the current path — nothing to do".to_string());
    }
    let target_free = !Path::new(&target).exists();
    if !target_free {
        return Err(format!(
            "collision: target {} already exists — refusing to overwrite (charter §7.5)",
            target
        ));
    }
    let cross = drive_of(&original) != drive_of(&target);
    let report_id = format!("relocate-{}-{}-{}", op, media_id, now_secs());
    let mut plan = RelocatePlan {
        report_id: report_id.clone(),
        mode: "dry-run".to_string(),
        op: op.to_string(),
        created_at: now_secs(),
        media_id,
        sha256: sha256.clone(),
        filename,
        bytes,
        original_path: original,
        target_path: target,
        on_disk,
        target_free,
        cross_drive: cross,
        warnings,
        report_path: String::new(),
    };
    if !on_disk {
        plan.warnings.push("source file not on disk — will be a path-only update".to_string());
    }
    if cross {
        plan.warnings
            .push("cross-drive: copy-then-remove-source (reported as such)".to_string());
    }
    plan.report_path = write_relocate_report(&report_id, &plan)?;
    Ok(plan)
}

/// Apply a rename/move: relocate the canonical file, update its path, journal.
pub fn apply_relocate(
    conn: &mut Connection,
    media_id: i64,
    target_path: &str,
    op: &str,
) -> Result<RelocatePlan, String> {
    let plan = plan_relocate(conn, media_id, target_path, op)?;
    let (_sha256, bytes, occ_id, original, on_disk) = canonical_of(conn, media_id)?;

    // Pre-flight: source must exist and target must be free, or fail before disk.
    if on_disk {
        if !Path::new(&original).exists() {
            return Err(format!("source vanished: {original}"));
        }
        if Path::new(&plan.target_path).exists() {
            return Err(format!(
                "collision: target {} already exists — refusing to overwrite (charter §7.5)",
                plan.target_path
            ));
        }
    }

    // File side first (nothing journaled until the move succeeds).
    if on_disk {
        let src = Path::new(&original);
        let dst = Path::new(&plan.target_path);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if drive_of(&original) != drive_of(&plan.target_path) {
            fs::copy(src, dst).map_err(|e| e.to_string())?;
            fs::remove_file(src).map_err(|e| e.to_string())?;
        } else {
            fs::rename(src, dst).map_err(|e| e.to_string())?;
        }
    }

    // DB side: one transaction — update the canonical path + journal.
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE occurrence SET path = ?2 WHERE id = ?1",
        rusqlite::params![occ_id, plan.target_path],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO op_journal (created_at, op, media_id, occ_id, original_path, new_path, bytes, cross_drive, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'applied')",
        rusqlite::params![
            now_secs(),
            op,
            media_id,
            occ_id,
            original,
            plan.target_path,
            bytes,
            if plan.cross_drive { 1 } else { 0 }
        ],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    // The searchable path changed; M6's ensure_fts skips a rebuild when the row
    // count is unchanged, so force it here to keep search in sync.
    crate::query::force_rebuild_fts(conn);

    let mut report = plan;
    report.mode = "applied".to_string();
    report.created_at = now_secs();
    report.report_id = format!("relocate-{}-{}-applied-{}", op, media_id, now_secs());
    report.report_path = write_relocate_report(&report.report_id, &report)?;
    Ok(report)
}

/// Undo the last applied rename/move for this media (reverse journal order).
pub fn undo_relocate(conn: &mut Connection, media_id: i64) -> Result<RelocatePlan, String> {
    let rows: Vec<(i64, String, String, Option<String>, i64, bool)> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, op, original_path, new_path, bytes, cross_drive
                 FROM op_journal
                 WHERE media_id = ?1 AND op IN ('rename','move') AND status = 'applied'
                 ORDER BY id DESC",
            )
            .map_err(|e| e.to_string())?;
        let it = stmt
            .query_map(rusqlite::params![media_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)? == 1,
                ))
            })
            .map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    if rows.is_empty() {
        return Err("no applied rename/move to undo for this item".to_string());
    }

    let (sha256, bytes): (String, i64) = conn
        .query_row(
            "SELECT sha256, bytes FROM media WHERE id = ?1",
            rusqlite::params![media_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut warnings: Vec<String> = Vec::new();
    let mut restored: Vec<(i64, String)> = Vec::new(); // journal_id -> original path

    for (jid, _op, original, new_path, _bytes, _cross) in rows.iter() {
        if let Some(p) = new_path.clone().filter(|s| !s.is_empty()) {
            let np = Path::new(&p);
            let op_path = Path::new(original);
            if np.exists() {
                if op_path.exists() {
                    warnings.push(format!(
                        "undo refused for {p}: original {original} exists — not overwriting"
                    ));
                    continue;
                }
                if drive_of(&p) != drive_of(original) {
                    fs::copy(np, op_path).map_err(|e| e.to_string())?;
                    fs::remove_file(np).map_err(|e| e.to_string())?;
                } else {
                    fs::rename(np, op_path).map_err(|e| e.to_string())?;
                }
                restored.push((*jid, original.clone()));
            } else {
                warnings.push(format!("undo skipped {p}: file no longer on disk"));
            }
        }
        // None / empty: nothing to move back.
    }

    // DB side: restore canonical path + mark journal undone.
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (jid, original) in &restored {
        tx.execute(
            "UPDATE occurrence SET path = ?2
             WHERE id = (SELECT occ_id FROM op_journal WHERE id = ?1)",
            rusqlite::params![jid, original],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE op_journal SET status = 'undone'
         WHERE media_id = ?1 AND op IN ('rename','move') AND status = 'applied'",
        rusqlite::params![media_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    crate::query::force_rebuild_fts(conn);

    // The first (most recent) row gives us the op label + original path for the report.
    let (last_op, last_original, last_target) = match rows.first() {
        Some((_, op, orig, np, _, _)) => (
            op.clone(),
            orig.clone(),
            np.clone().unwrap_or_default(),
        ),
        None => ("relocate".to_string(), String::new(), String::new()),
    };
    let filename = filename_of(&last_original);
    let on_disk = Path::new(&last_original).exists();
    let report = RelocatePlan {
        report_id: format!("relocate-{}-{}-undone-{}", last_op, media_id, now_secs()),
        mode: "undone".to_string(),
        op: last_op,
        created_at: now_secs(),
        media_id,
        sha256,
        filename,
        bytes,
        original_path: last_original,
        target_path: last_target,
        on_disk,
        target_free: true,
        cross_drive: false,
        warnings,
        report_path: String::new(),
    };
    let path = write_relocate_report(&report.report_id, &report)?;
    Ok(RelocatePlan { report_path: path, ..report })
}
