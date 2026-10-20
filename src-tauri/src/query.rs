//! M6: query layer — structured filters + FTS5 search + facets.
//!
//! One filtered query (apply_filter) backs both the Tauri `query` command and
//! the browser JSON API, so the counts are identical in both. Facets drive the
//! filter UI with real per-value counts. The FTS5 table is rebuilt lazily and
//! idempotently (measured ~20 ms) so it is always in sync with `media`.

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};
use serde::{Deserialize, Serialize};

use crate::scanner::Config;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i64,
    pub sha256: String,
    pub kind: String,
    pub ext: String,
    pub bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<String>,
    pub date_source: Option<String>,
    pub camera: Option<String>,
    pub path: String,
    pub occ_count: i64,
    /// Copies currently on disk (what the ×N duplicate badge shows).
    pub copies: i64,
    pub in_dup_group: bool,
    pub missing: bool,
    pub thumb_path: Option<String>,
    // --- annotations (library management) ---
    pub rating: i64,
    pub favorite: bool,
    pub color_label: Option<String>,
    pub has_notes: bool,
    /// Tag names, comma-joined (tile tooltip + CSV export).
    pub tags: Option<String>,
    /// Sent to the Recycle Bin from the app (restorable).
    pub deleted: bool,
    pub ai_reviewed: bool,
    /// AI suggestions waiting for approval
    pub ai_pending: i64,
    /// Sort "similar" only: 1-based look-alike group number and its size
    /// (absent for pictures with no look-alike).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sim_group: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sim_size: Option<i64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub id: i64,
    pub path: String,
    pub root: String,
    pub mtime: i64,
    pub missing: bool,
    pub dup_group: Option<i64>,
    pub is_canonical: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub item: Item,
    pub occurrences: Vec<Occurrence>,
    pub annotation: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sort {
    #[serde(alias = "takenDesc")]
    TakenDesc,
    #[serde(alias = "takenAsc")]
    TakenAsc,
    #[serde(alias = "nameAsc")]
    NameAsc,
    #[serde(alias = "sizeDesc")]
    SizeDesc,
    #[serde(alias = "ratingDesc")]
    RatingDesc,
    #[serde(alias = "editedDesc")]
    EditedDesc,
    /// Look-alike pictures next to each other (perceptual fingerprint).
    Similar,
}

impl Default for Sort {
    fn default() -> Self {
        Sort::TakenDesc
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Filter {
    /// "image" | "video" | "all" (default all)
    pub kind: Option<String>,
    /// exact year, e.g. "2018"
    pub year: Option<String>,
    /// decade start year, e.g. "2010" matches 2010-2019
    pub decade: Option<String>,
    /// camera contains (case-insensitive)
    pub camera: Option<String>,
    /// top-level folder (canonical path prefix), case-insensitive
    pub folder: Option<String>,
    pub min_width: Option<i64>,
    pub min_height: Option<i64>,
    /// free-text search -> FTS5 prefix terms (filename, folder, camera,
    /// notes, tags, categories, albums)
    pub search: Option<String>,
    pub sort: Option<Sort>,
    /// Sort "similar": max fingerprint difference (bits of 64) for two
    /// pictures to count as look-alikes. Default 10.
    pub sim_threshold: Option<u32>,
    pub limit: Option<usize>,
    // --- library management filters ---
    /// tag ids; with `tag_mode` "any" an item needs one of them, else all.
    pub tags: Option<Vec<i64>>,
    pub tag_mode: Option<String>,
    /// all tags in this category (any of them)
    pub category: Option<i64>,
    /// HIDE items carrying any of these tags
    pub exclude_tags: Option<Vec<i64>>,
    /// HIDE items carrying any tag in these categories
    pub exclude_categories: Option<Vec<i64>>,
    pub album: Option<i64>,
    pub untagged: Option<bool>,
    /// HIDE every item that has any tag (a toggle that stays on across views)
    pub hide_tagged: Option<bool>,
    pub has_notes: Option<bool>,
    pub favorite: Option<bool>,
    pub min_rating: Option<i64>,
    pub color: Option<String>,
    /// taken_at range, "YYYY-MM-DD" inclusive
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// "present" (default) | "missing" | "all"
    pub presence: Option<String>,
    /// only items in a duplicate group
    pub dups_only: Option<bool>,
    /// true: reviewed by the AI; false: not reviewed yet
    pub ai_reviewed: Option<bool>,
    /// only items with AI suggestions waiting for approval
    pub ai_pending: Option<bool>,
    /// explicit id list (assistant results, "show these")
    pub ids: Option<Vec<i64>>,
    /// exact folder (absolute path); items directly inside it
    pub dir: Option<String>,
    /// library folder (Settings → Library folders) by name: items with a copy there
    pub root: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facet {
    pub value: String,
    pub count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub total: i64,
    pub kinds: Vec<Facet>,
    pub years: Vec<Facet>,
    pub cameras: Vec<Facet>,
    pub folders: Vec<Facet>,
}

// ---------------------------------------------------------------------------
// FTS5
// ---------------------------------------------------------------------------

const FTS_COLUMNS: &str = "path, camera, ext, notes, tags, albums, description";

/// Ensure `media_fts` exists with the current column set and matches the
/// `media` row count. A full rebuild is cheap (tens of ms for ~10k rows).
pub fn ensure_fts(conn: &Connection) {
    let media_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0))
        .unwrap_or(-1);
    // Probe for the newest column: catalogs from before notes/tags rebuild once.
    let current_schema = conn.prepare("SELECT description FROM media_fts LIMIT 0").is_ok();
    let fts_rows: i64 = if current_schema {
        conn.query_row("SELECT COUNT(*) FROM media_fts", [], |r| r.get(0))
            .unwrap_or(-1)
    } else {
        -1
    };
    if current_schema && fts_rows == media_rows {
        return; // up to date
    }
    build_fts(conn);
}

/// Rebuild `media_fts` unconditionally (used after a rename/move, where the
/// row count is unchanged but the indexed path is stale).
pub fn force_rebuild_fts(conn: &Connection) {
    build_fts(conn);
}

/// The searchable text for media rows: present path (folder + filename),
/// camera, extension, notes, "category tag" pairs and album names.
const FTS_SELECT: &str = "SELECT m.id,
        COALESCE(
          (SELECT o.path FROM occurrence o WHERE o.media_id=m.id AND o.missing=0 ORDER BY o.is_canonical DESC, o.id LIMIT 1),
          (SELECT o.path FROM occurrence o WHERE o.media_id=m.id ORDER BY o.id LIMIT 1)
        ),
        m.camera, m.ext,
        (SELECT a.notes FROM annotations a WHERE a.media_id=m.id),
        (SELECT group_concat(COALESCE(c.name || ' ', '') || t.name, ' ; ')
           FROM media_tags mt JOIN tags t ON t.id=mt.tag_id LEFT JOIN categories c ON c.id=t.category_id
          WHERE mt.media_id=m.id),
        (SELECT group_concat(al.name, ' ; ') FROM album_items ai JOIN albums al ON al.id=ai.album_id
          WHERE ai.media_id=m.id),
        (SELECT a.ai_description FROM annotations a WHERE a.media_id=m.id)
      FROM media m";

fn build_fts(conn: &Connection) {
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS media_fts;
         CREATE VIRTUAL TABLE media_fts USING fts5({FTS_COLUMNS}, tokenize = 'unicode61 remove_diacritics 2');
         INSERT INTO media_fts(rowid, {FTS_COLUMNS}) {FTS_SELECT};"
    ))
    .expect("build media_fts");
}

/// Re-index specific items after their notes/tags/albums change (no full
/// rebuild, so typing a note stays instant).
pub fn fts_refresh(conn: &Connection, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    if conn.prepare("SELECT description FROM media_fts LIMIT 0").is_err() {
        build_fts(conn);
        return;
    }
    for chunk in ids.chunks(500) {
        let ph = vec!["?"; chunk.len()].join(",");
        let _ = conn.execute(
            &format!("DELETE FROM media_fts WHERE rowid IN ({ph})"),
            params_from_iter(chunk.iter()),
        );
        let _ = conn.execute(
            &format!("INSERT INTO media_fts(rowid, {FTS_COLUMNS}) {FTS_SELECT} WHERE m.id IN ({ph})"),
            params_from_iter(chunk.iter()),
        );
    }
}

/// Convert user free-text into a safe FTS5 query.
///   red car         -> "red"* AND "car"*     (every word, prefix match)
///   "red car"       -> "red car"             (exact phrase)
///   boat -sold      -> "boat"* NOT "sold"*   (exclude a word)
pub fn fts_query(user: &str) -> String {
    let mut pos: Vec<String> = vec![];
    let mut neg: Vec<String> = vec![];
    let mut rest = user.trim();
    while !rest.is_empty() {
        let negate = rest.starts_with('-') && rest.len() > 1;
        let r = if negate { &rest[1..] } else { rest };
        let (term, phrase, next) = if let Some(stripped) = r.strip_prefix('"') {
            match stripped.find('"') {
                Some(end) => (&stripped[..end], true, &stripped[end + 1..]),
                None => (stripped, true, ""),
            }
        } else {
            match r.find(char::is_whitespace) {
                Some(end) => (&r[..end], false, &r[end..]),
                None => (r, false, ""),
            }
        };
        let clean: String = term.chars().filter(|c| *c != '"').collect();
        let clean = clean.trim();
        if !clean.is_empty() {
            let q = if phrase { format!("\"{clean}\"") } else { format!("\"{clean}\"*") };
            if negate { neg.push(q) } else { pos.push(q) }
        }
        rest = next.trim_start();
    }
    if pos.is_empty() {
        return String::new(); // FTS5 NOT needs a left operand
    }
    let mut q = pos.join(" AND ");
    for n in neg {
        q = format!("{q} NOT {n}");
    }
    q
}

// ---------------------------------------------------------------------------
// Query builder
// ---------------------------------------------------------------------------

fn order_clause(sort: Sort) -> &'static str {
    match sort {
        Sort::TakenDesc => "m.taken_at DESC, m.id DESC",
        Sort::TakenAsc => "m.taken_at ASC, m.id ASC",
        Sort::NameAsc => "cpath COLLATE NOCASE ASC, m.id ASC",
        Sort::SizeDesc => "m.bytes DESC, m.id DESC",
        Sort::RatingDesc => "rating DESC, m.taken_at DESC, m.id DESC",
        Sort::EditedDesc => "COALESCE(ann.updated_at, 0) DESC, m.id DESC",
        // Base order; `order_by_similarity` regroups afterwards.
        Sort::Similar => "m.taken_at ASC, m.id ASC",
    }
}

/// Build the WHERE clause + params for a filter. Returns ("", []) for no filter.
fn build_where(filter: &Filter) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    macro_rules! add {
        ($cond:expr, $val:expr) => {{
            params.push($val);
            clauses.push(format!("({} ?{})", $cond, params.len()));
        }};
    }

    if let Some(k) = &filter.kind {
        if k != "all" && !k.is_empty() {
            add!("m.kind =", Value::Text(k.clone()));
        }
    }
    if let Some(y) = &filter.year {
        if !y.is_empty() {
            add!("substr(m.taken_at,1,4) =", Value::Text(y.clone()));
        }
    }
    if let Some(d) = &filter.decade {
        if let Ok(start) = d.parse::<i32>() {
            add!("substr(m.taken_at,1,4) >=", Value::Text(start.to_string()));
            add!("substr(m.taken_at,1,4) <=", Value::Text((start + 9).to_string()));
        }
    }
    if let Some(cam) = &filter.camera {
        if !cam.is_empty() {
            add!("lower(m.camera) LIKE", Value::Text(format!("%{}%", cam.to_lowercase())));
        }
    }
    if let Some(fold) = &filter.folder {
        if !fold.is_empty() {
            // EXISTS needs its parameter inside the subquery, so build it
            // explicitly rather than via the `add!` macro (which appends at end).
            params.push(Value::Text(format!("%{}%", fold.to_lowercase())));
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.is_canonical=1 AND lower(o.path) LIKE ?{})",
                params.len()
            ));
        }
    }
    if let Some(w) = filter.min_width {
        add!("m.width >=", Value::Integer(w));
    }
    if let Some(h) = filter.min_height {
        add!("m.height >=", Value::Integer(h));
    }
    if let Some(s) = &filter.search {
        let q = fts_query(s);
        if !q.is_empty() {
            // MATCH parameter must sit inside the subquery, so build explicitly.
            params.push(Value::Text(q));
            clauses.push(format!(
                "m.id IN (SELECT rowid FROM media_fts WHERE media_fts MATCH ?{})",
                params.len()
            ));
        }
    }

    // --- library management filters ---
    if let Some(tags) = &filter.tags {
        if !tags.is_empty() {
            let any = filter.tag_mode.as_deref() == Some("any");
            if any {
                let mut ph = vec![];
                for t in tags {
                    params.push(Value::Integer(*t));
                    ph.push(format!("?{}", params.len()));
                }
                clauses.push(format!(
                    "EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id=m.id AND mt.tag_id IN ({}))",
                    ph.join(",")
                ));
            } else {
                for t in tags {
                    params.push(Value::Integer(*t));
                    clauses.push(format!(
                        "EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id=m.id AND mt.tag_id = ?{})",
                        params.len()
                    ));
                }
            }
        }
    }
    if let Some(c) = filter.category {
        params.push(Value::Integer(c));
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM media_tags mt JOIN tags t ON t.id=mt.tag_id WHERE mt.media_id=m.id AND t.category_id = ?{})",
            params.len()
        ));
    }
    if let Some(ex) = &filter.exclude_tags {
        if !ex.is_empty() {
            let mut ph = vec![];
            for t in ex {
                params.push(Value::Integer(*t));
                ph.push(format!("?{}", params.len()));
            }
            clauses.push(format!(
                "NOT EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id=m.id AND mt.tag_id IN ({}))",
                ph.join(",")
            ));
        }
    }
    if let Some(ex) = &filter.exclude_categories {
        if !ex.is_empty() {
            let mut ph = vec![];
            for c in ex {
                params.push(Value::Integer(*c));
                ph.push(format!("?{}", params.len()));
            }
            clauses.push(format!(
                "NOT EXISTS (SELECT 1 FROM media_tags mt JOIN tags t ON t.id=mt.tag_id
                   WHERE mt.media_id=m.id AND t.category_id IN ({}))",
                ph.join(",")
            ));
        }
    }
    if let Some(a) = filter.album {
        params.push(Value::Integer(a));
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM album_items ai WHERE ai.media_id=m.id AND ai.album_id = ?{})",
            params.len()
        ));
    }
    if filter.untagged == Some(true) || filter.hide_tagged == Some(true) {
        clauses.push("NOT EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id=m.id)".into());
    }
    if let Some(h) = filter.has_notes {
        clauses.push(if h { "COALESCE(ann.notes,'') <> ''".into() } else { "COALESCE(ann.notes,'') = ''".into() });
    }
    if filter.favorite == Some(true) {
        clauses.push("COALESCE(ann.favorite,0) = 1".into());
    }
    if let Some(r) = filter.min_rating {
        if r > 0 {
            add!("COALESCE(ann.rating,0) >=", Value::Integer(r));
        }
    }
    if let Some(c) = &filter.color {
        if c == "none" {
            clauses.push("ann.color_label IS NULL".into());
        } else if !c.is_empty() {
            add!("ann.color_label =", Value::Text(c.clone()));
        }
    }
    if let Some(d) = &filter.date_from {
        if !d.is_empty() {
            add!("m.taken_at >=", Value::Text(d.clone()));
        }
    }
    if let Some(d) = &filter.date_to {
        if !d.is_empty() {
            // inclusive end date: everything before the next day
            add!("m.taken_at <", Value::Text(format!("{d}\u{ffff}")));
        }
    }
    match filter.presence.as_deref() {
        Some("all") => {}
        Some("missing") => clauses.push(
            "m.deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0)".into(),
        ),
        Some("deleted") => clauses.push(
            "m.deleted_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0)".into(),
        ),
        _ => clauses.push(
            "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0)".into(),
        ),
    }
    if let Some(r) = filter.ai_reviewed {
        clauses.push(if r { "ann.ai_reviewed_at IS NOT NULL".into() } else { "ann.ai_reviewed_at IS NULL".into() });
    }
    if filter.ai_pending == Some(true) {
        clauses.push("EXISTS (SELECT 1 FROM ai_suggestions s WHERE s.media_id=m.id AND s.status='pending')".into());
    }
    if let Some(list) = &filter.ids {
        let csv: Vec<String> = list.iter().map(|i| i.to_string()).collect(); // integers only: safe inline
        clauses.push(format!("m.id IN ({})", if csv.is_empty() { "NULL".to_string() } else { csv.join(",") }));
    }
    if filter.dups_only == Some(true) {
        clauses.push(
            "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0 AND o.dup_group IS NOT NULL)".into(),
        );
    }
    if let Some(r) = &filter.root {
        if !r.is_empty() {
            params.push(Value::Text(r.clone()));
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0 AND o.root = ?{})",
                params.len()
            ));
        }
    }
    if let Some(d) = &filter.dir {
        if !d.is_empty() {
            // Exact folder match without LIKE (underscores in folder names are
            // LIKE wildcards): prefix compare + no further separator.
            let k = crate::paths::key(d);
            let k = format!("{}\\", k.trim_end_matches('\\'));
            let n = k.chars().count() as i64;
            params.push(Value::Text(k));
            let p1 = params.len();
            params.push(Value::Integer(n));
            let p2 = params.len();
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id=m.id AND o.missing=0
                   AND substr(lower(replace(o.path,'/','\\')), 1, ?{p2}) = ?{p1}
                   AND instr(substr(replace(o.path,'/','\\'), ?{p2} + 1), '\\') = 0)"
            ));
        }
    }

    (clauses.join(" AND "), params)
}

const ITEM_SELECT: &str = "SELECT m.id, m.sha256, m.kind, m.ext, m.bytes, m.width, m.height,
       m.taken_at, m.date_source, m.camera, m.thumb_path,
       COALESCE(
         (SELECT o.path FROM occurrence o WHERE o.media_id=m.id AND o.missing=0 AND o.is_canonical=1 ORDER BY o.id LIMIT 1),
         (SELECT o.path FROM occurrence o WHERE o.media_id=m.id AND o.missing=0 ORDER BY o.id LIMIT 1),
         (SELECT o.path FROM occurrence o WHERE o.media_id=m.id ORDER BY o.id LIMIT 1)
       ) AS cpath,
       (SELECT COUNT(*) FROM occurrence o2 WHERE o2.media_id=m.id) AS occ_count,
       (SELECT COUNT(*) FROM occurrence o3 WHERE o3.media_id=m.id AND o3.missing=0) AS present_count,
       EXISTS(SELECT 1 FROM occurrence o4 WHERE o4.media_id=m.id AND o4.dup_group IS NOT NULL AND o4.missing=0) AS in_dup,
       COALESCE(ann.rating, 0) AS rating,
       COALESCE(ann.favorite, 0) AS favorite,
       ann.color_label,
       COALESCE(ann.notes, '') <> '' AS has_notes,
       (SELECT group_concat(t.name, ', ') FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
         WHERE mt.media_id = m.id) AS tag_names,
       m.deleted_at IS NOT NULL AS deleted,
       ann.ai_reviewed_at IS NOT NULL AS ai_reviewed,
       (SELECT COUNT(*) FROM ai_suggestions s WHERE s.media_id = m.id AND s.status = 'pending') AS ai_pending
FROM media m LEFT JOIN annotations ann ON ann.media_id = m.id";

fn map_item(r: &rusqlite::Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        sha256: r.get(1)?,
        kind: r.get(2)?,
        ext: r.get(3)?,
        bytes: r.get(4)?,
        width: r.get(5)?,
        height: r.get(6)?,
        taken_at: r.get(7)?,
        date_source: r.get(8)?,
        camera: r.get(9)?,
        thumb_path: r.get::<_, Option<String>>(10)?,
        // NULL only for a media row with no occurrence at all (should not
        // happen after the upsert fix) — never let one row fail the grid.
        path: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
        occ_count: r.get(12)?,
        copies: r.get(13)?,
        missing: r.get::<_, i64>(13)? == 0, // true only when no copy is left on disk
        // A duplicate needs two copies ON DISK, whatever the stored group says.
        in_dup_group: r.get::<_, i64>(14)? == 1 && r.get::<_, i64>(13)? > 1,
        rating: r.get(15)?,
        favorite: r.get::<_, i64>(16)? == 1,
        color_label: r.get(17)?,
        has_notes: r.get::<_, i64>(18)? == 1,
        tags: r.get(19)?,
        deleted: r.get::<_, i64>(20)? == 1,
        ai_reviewed: r.get::<_, i64>(21)? == 1,
        ai_pending: r.get(22)?,
        sim_group: None,
        sim_size: None,
    })
}

/// Apply a filter and return matching items (sorted). `limit=None` = all.
pub fn apply_filter(conn: &Connection, filter: &Filter) -> Result<Vec<Item>, String> {
    ensure_fts(conn);
    let (where_sql, params) = build_where(filter);
    let sort = filter.sort.unwrap_or_default();
    let sql = format!(
        "{} {} {} {}",
        ITEM_SELECT,
        if where_sql.is_empty() { String::new() } else { format!("WHERE {where_sql}") },
        "ORDER BY",
        order_clause(sort),
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(params), map_item)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        let it = r.map_err(|e| e.to_string())?;
        out.push(it);
        // Similarity needs every match before it can order them.
        if let (Some(lim), false) = (filter.limit, sort == Sort::Similar) {
            if out.len() >= lim {
                break;
            }
        }
    }
    if sort == Sort::Similar {
        out = order_by_similarity(conn, out, filter.sim_threshold.unwrap_or(10).min(32));
        if let Some(lim) = filter.limit {
            out.truncate(lim);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Similarity order
// ---------------------------------------------------------------------------

/// Regroup `items` (given oldest first) so look-alike pictures sit together.
///
/// Pictures whose fingerprints (`media.phash`, see dedupe.rs) differ by at
/// most `max_bits` are linked; linked pictures form a group (chains allowed —
/// this is for browsing, nothing is removed). Groups come first, largest
/// first; inside a group each next picture is the closest one left to the
/// previous, so near-identical shots end up side by side. Pictures with no
/// look-alike follow in date order, then videos and unfingerprinted files.
fn order_by_similarity(conn: &Connection, items: Vec<Item>, max_bits: u32) -> Vec<Item> {
    use rayon::prelude::*;
    use std::collections::HashMap;

    let hashes: HashMap<i64, u64> = conn
        .prepare("SELECT id, phash FROM media WHERE phash IS NOT NULL AND kind = 'image'")
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();

    let (hashed, rest): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|it| hashes.contains_key(&it.id));
    let h: Vec<u64> = hashed.iter().map(|it| hashes[&it.id]).collect();
    let dist = |a: usize, b: usize| (h[a] ^ h[b]).count_ones();
    let n = h.len();

    // Union-find over all close pairs, a block of rows at a time so a loose
    // threshold on a big library never holds every pair in memory.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for start in (0..n).step_by(512) {
        let pairs: Vec<(usize, usize)> = (start..(start + 512).min(n))
            .into_par_iter()
            .flat_map_iter(|i| (i + 1..n).filter(move |&j| dist(i, j) <= max_bits).map(move |j| (i, j)))
            .collect();
        for (i, j) in pairs {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    // Members are in date order (indexes ascending), so a group's first
    // member is its oldest picture.
    let mut groups: Vec<Vec<usize>> = groups.into_values().collect();
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));

    let mut order: Vec<(usize, Option<(i64, i64)>)> = Vec::with_capacity(n);
    let mut group_no = 0;
    for g in &groups {
        if g.len() < 2 {
            order.push((g[0], None));
            continue;
        }
        group_no += 1;
        let tag = Some((group_no, g.len() as i64));
        // Greedy nearest-neighbour chain from the oldest picture.
        let mut left: Vec<usize> = g[1..].to_vec();
        let mut cur = g[0];
        order.push((cur, tag));
        while !left.is_empty() {
            let k = (0..left.len()).min_by_key(|&k| (dist(cur, left[k]), left[k])).unwrap();
            cur = left.remove(k);
            order.push((cur, tag));
        }
    }

    let mut slots: Vec<Option<Item>> = hashed.into_iter().map(Some).collect();
    let mut out: Vec<Item> = Vec::with_capacity(slots.len() + rest.len());
    for (i, tag) in order {
        let mut it = slots[i].take().expect("each picture placed once");
        if let Some((g, size)) = tag {
            it.sim_group = Some(g);
            it.sim_size = Some(size);
        }
        out.push(it);
    }
    out.extend(rest);
    out
}

// ---------------------------------------------------------------------------
// Facets (real counts for the filter UI)
// ---------------------------------------------------------------------------

fn facet_rows(conn: &Connection, sql: &str) -> Vec<Facet> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(sql) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(Facet {
                value: r.get(0)?,
                count: r.get(1)?,
            })
        }) {
            out = rows.flatten().collect();
        }
    }
    out
}

pub fn facets(conn: &Connection, cfg: &Config) -> Facets {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM media", [], |r| r.get(0))
        .unwrap_or(0);
    let kinds = facet_rows(
        conn,
        "SELECT kind, COUNT(*) AS cnt FROM media GROUP BY kind ORDER BY cnt DESC",
    );
    let years = facet_rows(
        conn,
        "SELECT substr(taken_at,1,4) AS y, COUNT(*) AS cnt FROM media
         WHERE taken_at IS NOT NULL AND length(taken_at) >= 4
         GROUP BY y ORDER BY y DESC LIMIT 60",
    );
    let cameras = facet_rows(
        conn,
        "SELECT camera, COUNT(*) AS cnt FROM media
         WHERE camera IS NOT NULL AND camera <> ''
         GROUP BY camera ORDER BY cnt DESC LIMIT 25",
    );
    // First folder relative to the configured root, e.g. `Videos`, `takeout-...`.
    let folders = top_level_folders(conn, cfg);
    Facets {
        total,
        kinds,
        years,
        cameras,
        folders,
    }
}

/// Distinct first folder *relative to a configured root*, with counts.
/// e.g. root `E:/Photos`, path `E:\Photos\Videos\x.mp4` -> `Videos`.
/// Files directly in a root -> "(root)".
fn top_level_folders(conn: &Connection, cfg: &Config) -> Vec<Facet> {
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let roots: Vec<String> = cfg
        .roots
        .iter()
        .map(|r| r.path.replace('/', "\\").to_lowercase())
        .collect();

    if let Ok(mut stmt) = conn.prepare(
        "SELECT COALESCE(
           (SELECT o.path FROM occurrence o WHERE o.media_id=m.id AND o.is_canonical=1 ORDER BY o.id LIMIT 1),
           (SELECT o.path FROM occurrence o WHERE o.media_id=m.id ORDER BY o.id LIMIT 1)
         ) AS p
         FROM media m",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for p in rows.flatten() {
                if let Some(seg) = folder_relative_to_root(&p, &roots) {
                    *counts.entry(seg).or_insert(0) += 1;
                }
            }
        }
    }
    let mut out: Vec<Facet> = counts
        .into_iter()
        .map(|(value, count)| Facet { value, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
    out.truncate(40);
    out
}

/// First path segment after the matched configured root (case-insensitive
/// match, original-case label).
fn folder_relative_to_root(path: &str, roots: &[String]) -> Option<String> {
    // Preserve original case for the label; normalize separators.
    let orig = path.replace('/', "\\");
    let p = orig.to_lowercase();
    // Longest root prefix wins (handles nested roots), matched case-insensitively.
    let mut best: Option<usize> = None;
    for (i, r) in roots.iter().enumerate() {
        if r.is_empty() {
            continue;
        }
        if p.starts_with(r.as_str()) {
            let cur = best.map(|b| roots[b].len()).unwrap_or(0);
            if r.len() > cur {
                best = Some(i);
            }
        }
    }
    let rest = match best {
        Some(i) => orig[roots[i].len()..].trim_start_matches('\\').trim(),
        None => return None,
    };
    if rest.is_empty() {
        return Some("(root)".to_string());
    }
    let seg = rest.split('\\').next()?.trim().to_string();
    if seg.is_empty() {
        Some("(root)".to_string())
    } else {
        Some(seg)
    }
}

// ---------------------------------------------------------------------------
// Detail + stats (moved here so the API + Tauri share one implementation)
// ---------------------------------------------------------------------------

pub fn item_detail(conn: &Connection, id: i64) -> Result<Detail, String> {
    let item: Option<Item> = conn
        .query_row(
            format!("{} WHERE m.id = ?1", ITEM_SELECT).as_str(),
            rusqlite::params![id],
            map_item,
        )
        .ok();
    let item = item.ok_or("not found")?;

    let mut stmt = conn
        .prepare("SELECT id, path, root, mtime, missing, dup_group, is_canonical
                  FROM occurrence WHERE media_id = ?1 ORDER BY is_canonical DESC, id")
        .map_err(|e| e.to_string())?;
    let occs = stmt
        .query_map(rusqlite::params![id], |r| {
            Ok(Occurrence {
                id: r.get(0)?,
                path: r.get(1)?,
                root: r.get(2)?,
                mtime: r.get(3)?,
                missing: r.get::<_, i64>(4)? == 1,
                dup_group: r.get(5)?,
                is_canonical: r.get::<_, i64>(6)? == 1,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let annotation = crate::library::annotation(conn, id);
    Ok(Detail {
        item,
        occurrences: occs,
        annotation,
    })
}

pub fn stats(conn: &Connection) -> serde_json::Value {
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let total = q("SELECT COUNT(*) FROM media");
    let images = q("SELECT COUNT(*) FROM media m WHERE kind = 'image' AND EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)");
    let videos = q("SELECT COUNT(*) FROM media m WHERE kind = 'video' AND EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)");
    let missing = q("SELECT COUNT(*) FROM media m WHERE deleted_at IS NULL AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)");
    let deleted = q("SELECT COUNT(*) FROM media m WHERE deleted_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)");
    let dup_groups = q(
        "SELECT COUNT(DISTINCT dup_group) FROM occurrence WHERE dup_group IS NOT NULL AND missing = 0",
    );
    let present = q("SELECT COUNT(*) FROM media m WHERE EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)");
    // Counts match the sidebar views, which show items on disk only.
    const ON_DISK: &str = "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = x.media_id AND o.missing = 0)";
    let tagged = q(&format!("SELECT COUNT(DISTINCT media_id) FROM media_tags x WHERE {ON_DISK}"));
    let favorites = q(&format!("SELECT COUNT(*) FROM annotations x WHERE favorite = 1 AND {ON_DISK}"));
    let with_notes = q(&format!("SELECT COUNT(*) FROM annotations x WHERE notes <> '' AND {ON_DISK}"));
    // Duplicate groups across folders (exact + same photo at another quality),
    // as counted by the last duplicates scan.
    let dup_found: Option<serde_json::Value> = conn
        .query_row("SELECT value FROM app_meta WHERE key = 'dup_summary_cache'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    serde_json::json!({
        "total": total, "images": images, "videos": videos,
        "missing": missing, "dupGroups": dup_groups, "dupFound": dup_found,
        "present": present, "deleted": deleted, "tagged": tagged, "favorites": favorites, "withNotes": with_notes,
    })
}

#[cfg(test)]
mod tests {
    use super::fts_query;

    #[test]
    fn fts_query_forms() {
        assert_eq!(fts_query("red car"), r#""red"* AND "car"*"#);
        assert_eq!(fts_query(r#""key west" boat"#), r#""key west" AND "boat"*"#);
        assert_eq!(fts_query("boat -sold"), r#""boat"* NOT "sold"*"#);
        assert_eq!(fts_query("-sold"), "");
        assert_eq!(fts_query(r#"we"ird"#), r#""weird"*"#);
        assert_eq!(fts_query("   "), "");
        assert_eq!(fts_query(r#""unterminated phrase"#), r#""unterminated phrase""#);
    }

    fn item(id: i64) -> super::Item {
        super::Item {
            id, sha256: String::new(), kind: "image".into(), ext: "jpg".into(), bytes: 0,
            width: None, height: None, taken_at: None, date_source: None, camera: None,
            path: String::new(), occ_count: 1, copies: 1, in_dup_group: false, missing: false,
            thumb_path: None, rating: 0, favorite: false, color_label: None, has_notes: false,
            tags: None, deleted: false, ai_reviewed: false, ai_pending: 0, sim_group: None, sim_size: None,
        }
    }

    #[test]
    fn similarity_groups_look_alikes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE media (id INTEGER PRIMARY KEY, kind TEXT, phash INTEGER)").unwrap();
        // 1 and 3 are a pair, 4/2/6 a chain (4~2, 2~6 within 2 bits; 4..6 is 4 bits),
        // 5 is unique, 7 has no fingerprint.
        for (id, h) in [(1, 0x0000_0000_0000_00FFu64), (2, 0xFF00_0000_0000_0003), (3, 0x0000_0000_0000_00FE),
                        (4, 0xFF00_0000_0000_0000), (5, 0x00FF_FF00_0000_0000), (6, 0xFF00_0000_0000_000F)] {
            conn.execute("INSERT INTO media VALUES (?1, 'image', ?2)", rusqlite::params![id, h as i64]).unwrap();
        }
        let items = (1..=7).map(item).collect();
        let out = super::order_by_similarity(&conn, items, 2);
        let ids: Vec<i64> = out.iter().map(|i| i.id).collect();
        // Biggest group first, chained from its oldest picture by nearest neighbour.
        assert_eq!(ids, vec![2, 4, 6, 1, 3, 5, 7]);
        let groups: Vec<_> = out.iter().map(|i| (i.sim_group, i.sim_size)).collect();
        assert_eq!(groups[0], (Some(1), Some(3)));
        assert_eq!(groups[3], (Some(2), Some(2)));
        assert_eq!(groups[5], (None, None));
        assert_eq!(groups[6], (None, None));
    }
}
