//! Library management: categories + tags, notes, ratings / favorites / color
//! labels, albums, saved searches, bulk edits, catalog maintenance (sync,
//! cleanup, backup, CSV export) and an activity log.
//!
//! Everything is keyed on `media_id` (content identity), so annotations follow
//! a photo through renames and moves — whether done in this app or in Explorer.
//!
//! One entry point, `call(cmd, args)`, serves both the Tauri `call` command and
//! the browser-dev JSON API (`POST /api/call/<cmd>`), so both paths run the
//! exact same code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::scanner::Config;

pub const COLORS: &[&str] = &["red", "orange", "yellow", "green", "blue", "purple"];

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// Arg helpers
// ---------------------------------------------------------------------------

fn ids(a: &Value) -> Result<Vec<i64>, String> {
    let v = a
        .get("ids")
        .and_then(|v| v.as_array())
        .ok_or("missing ids")?;
    let out: Vec<i64> = v.iter().filter_map(|x| x.as_i64()).collect();
    if out.is_empty() {
        return Err("no items selected".into());
    }
    Ok(out)
}
fn int(a: &Value, k: &str) -> Result<i64, String> {
    a.get(k).and_then(|v| v.as_i64()).ok_or_else(|| format!("missing {k}"))
}
fn opt_int(a: &Value, k: &str) -> Option<i64> {
    a.get(k).and_then(|v| v.as_i64())
}
fn text(a: &Value, k: &str) -> Result<String, String> {
    a.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing {k}"))
}
fn name(a: &Value, k: &str) -> Result<String, String> {
    let s = text(a, k)?.trim().to_string();
    if s.is_empty() {
        return Err(format!("{k} cannot be empty"));
    }
    if s.len() > 200 {
        return Err(format!("{k} is too long"));
    }
    Ok(s)
}
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

pub(crate) fn log_activity(conn: &Connection, action: &str, count: usize, detail: &str) {
    log(conn, action, count, detail)
}

fn log(conn: &Connection, action: &str, count: usize, detail: &str) {
    let _ = conn.execute(
        "INSERT INTO activity_log (at, action, media_count, detail) VALUES (?1,?2,?3,?4)",
        params![now(), action, count as i64, detail],
    );
}

fn ensure_annotation_rows(conn: &Connection, ids: &[i64]) -> Result<(), String> {
    let mut st = conn
        .prepare("INSERT OR IGNORE INTO annotations (media_id, updated_at) VALUES (?1, ?2)")
        .map_err(|e| e.to_string())?;
    let t = now();
    for id in ids {
        st.execute(params![id, t]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn touch(conn: &Connection, ids: &[i64]) {
    let sql = format!(
        "UPDATE annotations SET updated_at = ?1 WHERE media_id IN ({})",
        placeholders(ids.len())
    );
    let mut p: Vec<rusqlite::types::Value> = vec![now().into()];
    p.extend(ids.iter().map(|i| (*i).into()));
    let _ = conn.execute(&sql, params_from_iter(p));
}

// ---------------------------------------------------------------------------
// Annotations
// ---------------------------------------------------------------------------

/// Full annotation block for one item (detail panel).
pub fn annotation(conn: &Connection, media_id: i64) -> Value {
    let (notes, rating, favorite, color, updated, ai_desc, ai_at): (String, i64, i64, Option<String>, Option<i64>, String, Option<i64>) = conn
        .query_row(
            "SELECT notes, rating, favorite, color_label, updated_at, ai_description, ai_reviewed_at FROM annotations WHERE media_id = ?1",
            [media_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap_or_else(|_| (String::new(), 0, 0, None, None, String::new(), None));
    let tags = item_tags(conn, &[media_id]);
    let albums: Vec<Value> = conn
        .prepare(
            "SELECT a.id, a.name FROM album_items ai JOIN albums a ON a.id = ai.album_id
             WHERE ai.media_id = ?1 ORDER BY a.name COLLATE NOCASE",
        )
        .and_then(|mut st| {
            st.query_map([media_id], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)? }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    json!({
        "mediaId": media_id,
        "notes": notes,
        "rating": rating,
        "favorite": favorite == 1,
        "colorLabel": color,
        "updatedAt": updated,
        "aiDescription": ai_desc,
        "aiReviewedAt": ai_at,
        "tags": tags,
        "albums": albums,
    })
}

/// Tags present on a set of items, with how many of them carry each tag.
fn item_tags(conn: &Connection, ids: &[i64]) -> Vec<Value> {
    if ids.is_empty() {
        return vec![];
    }
    let sql = format!(
        "SELECT t.id, t.name, c.id, c.name, c.color, COUNT(*) AS n
         FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
         LEFT JOIN categories c ON c.id = t.category_id
         WHERE mt.media_id IN ({})
         GROUP BY t.id ORDER BY c.sort, c.name COLLATE NOCASE, t.name COLLATE NOCASE",
        placeholders(ids.len())
    );
    conn.prepare(&sql)
        .and_then(|mut st| {
            st.query_map(params_from_iter(ids.iter()), |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "categoryId": r.get::<_, Option<i64>>(2)?,
                    "category": r.get::<_, Option<String>>(3)?,
                    "color": r.get::<_, Option<String>>(4)?,
                    "count": r.get::<_, i64>(5)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default()
}

fn ann_text(conn: &Connection, id: i64, col: &str) -> String {
    conn.query_row(&format!("SELECT {col} FROM annotations WHERE media_id = ?1"), [id], |r| r.get(0))
        .unwrap_or_default()
}

fn set_notes(conn: &Connection, id: i64, notes: &str) -> Result<Value, String> {
    if notes.len() > 100_000 {
        return Err("note is too long (100,000 characters max)".into());
    }
    ensure_annotation_rows(conn, &[id])?;
    let before = ann_text(conn, id, "notes");
    if before != notes {
        crate::history::record(conn, id, "notes", json!({ "before": before, "after": notes }));
    }
    conn.execute(
        "UPDATE annotations SET notes = ?2, updated_at = ?3 WHERE media_id = ?1",
        params![id, notes, now()],
    )
    .map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, &[id]);
    log(conn, "notes", 1, "");
    Ok(annotation(conn, id))
}

pub(crate) fn append_notes(conn: &mut Connection, ids: &[i64], add: &str) -> Result<Value, String> {
    let add = add.trim();
    if add.is_empty() {
        return Err("nothing to append".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    ensure_annotation_rows(&tx, ids)?;
    {
        let mut st = tx
            .prepare(
                "UPDATE annotations SET notes = CASE WHEN notes = '' THEN ?2 ELSE notes || char(10) || ?2 END,
                        updated_at = ?3 WHERE media_id = ?1",
            )
            .map_err(|e| e.to_string())?;
        let t = now();
        for id in ids {
            let before = ann_text(&tx, *id, "notes");
            st.execute(params![id, add, t]).map_err(|e| e.to_string())?;
            let after = ann_text(&tx, *id, "notes");
            crate::history::record(&tx, *id, "notes", json!({ "before": before, "after": after }));
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, ids);
    log(conn, "notes.append", ids.len(), add);
    Ok(json!({ "updated": ids.len() }))
}

pub(crate) fn set_field(conn: &mut Connection, ids: &[i64], col: &str, val: rusqlite::types::Value) -> Result<Value, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    ensure_annotation_rows(&tx, ids)?;
    // History: one entry per item whose value actually changes.
    let action = match col { "color_label" => "color", c => c };
    let after = crate::history_value(&val);
    {
        let mut st = tx
            .prepare(&format!("SELECT {col} FROM annotations WHERE media_id = ?1"))
            .map_err(|e| e.to_string())?;
        for id in ids {
            let before: rusqlite::types::Value = st.query_row([id], |r| r.get(0)).unwrap_or(rusqlite::types::Value::Null);
            let before = crate::history_value(&before);
            if before != after {
                crate::history::record(&tx, *id, action, json!({ "before": before, "after": after }));
            }
        }
    }
    let sql = format!(
        "UPDATE annotations SET {col} = ?1, updated_at = ?2 WHERE media_id IN ({})",
        placeholders(ids.len())
    );
    let mut p: Vec<rusqlite::types::Value> = vec![val.clone(), now().into()];
    p.extend(ids.iter().map(|i| (*i).into()));
    tx.execute(&sql, params_from_iter(p)).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    log(conn, col, ids.len(), &format!("{val:?}"));
    Ok(json!({ "updated": ids.len() }))
}

/// The AI-written description (separate from the user's notes; searchable).
pub(crate) fn set_description(conn: &Connection, id: i64, text: &str) -> Result<(), String> {
    ensure_annotation_rows(conn, &[id])?;
    let before = ann_text(conn, id, "ai_description");
    if before == text {
        return Ok(());
    }
    crate::history::record(conn, id, "description", json!({ "before": before, "after": text }));
    conn.execute(
        "UPDATE annotations SET ai_description = ?2, updated_at = ?3 WHERE media_id = ?1",
        params![id, text, now()],
    )
    .map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, &[id]);
    Ok(())
}

pub(crate) fn ensure_album(conn: &Connection, name: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT OR IGNORE INTO albums (name, description, created_at) VALUES (?1, '', ?2)",
        params![name, now()],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row("SELECT id FROM albums WHERE name = ?1", [name], |r| r.get(0)).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Categories + tags
// ---------------------------------------------------------------------------

fn ensure_category(conn: &Connection, name: &str) -> Result<i64, String> {
    conn.execute("INSERT OR IGNORE INTO categories (name) VALUES (?1)", [name])
        .map_err(|e| e.to_string())?;
    conn.query_row("SELECT id FROM categories WHERE name = ?1", [name], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// Create the tag if needed. `"Category: tag"` puts a NEW tag in that category
/// (creating the category); an existing tag keeps its category unless one is
/// given explicitly.
pub(crate) fn ensure_tag(conn: &Connection, raw: &str, category_id: Option<i64>) -> Result<i64, String> {
    let (cat, tag) = match raw.split_once(':') {
        Some((c, t)) if !c.trim().is_empty() && !t.trim().is_empty() => (Some(c.trim()), t.trim()),
        _ => (None, raw.trim()),
    };
    if tag.is_empty() {
        return Err("empty tag".into());
    }
    if tag.len() > 100 {
        return Err("tag is too long".into());
    }
    let cat_id = match (cat, category_id) {
        (Some(c), _) => Some(ensure_category(conn, c)?),
        (None, c) => c,
    };
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", [tag], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    match existing {
        Some(id) => {
            if let Some(c) = cat_id {
                conn.execute("UPDATE tags SET category_id = ?2 WHERE id = ?1", params![id, c])
                    .map_err(|e| e.to_string())?;
            }
            Ok(id)
        }
        None => {
            conn.execute("INSERT INTO tags (name, category_id) VALUES (?1, ?2)", params![tag, cat_id])
                .map_err(|e| e.to_string())?;
            Ok(conn.last_insert_rowid())
        }
    }
}

pub(crate) fn add_tags(conn: &mut Connection, ids: &[i64], names: &[String], category_id: Option<i64>) -> Result<Value, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut tag_ids = vec![];
    for n in names {
        if n.trim().is_empty() {
            continue;
        }
        tag_ids.push(ensure_tag(&tx, n, category_id)?);
    }
    if tag_ids.is_empty() {
        return Err("no tags given".into());
    }
    {
        let mut st = tx
            .prepare("INSERT OR IGNORE INTO media_tags (media_id, tag_id) VALUES (?1, ?2)")
            .map_err(|e| e.to_string())?;
        let labels: Vec<(String, Option<String>)> = tag_ids.iter().map(|t| tag_label(&tx, *t)).collect();
        for m in ids {
            for (t, (name, cat)) in tag_ids.iter().zip(&labels) {
                if st.execute(params![m, t]).map_err(|e| e.to_string())? > 0 {
                    crate::history::record(&tx, *m, "tag_add", json!({ "tagId": t, "name": name, "category": cat }));
                }
            }
        }
    }
    ensure_annotation_rows(&tx, ids)?;
    touch(&tx, ids);
    tx.commit().map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, ids);
    log(conn, "tags.add", ids.len(), &names.join(", "));
    Ok(json!({ "updated": ids.len(), "tagIds": tag_ids }))
}

/// (name, category name) of a tag, for history entries.
fn tag_label(conn: &Connection, tag_id: i64) -> (String, Option<String>) {
    conn.query_row(
        "SELECT t.name, c.name FROM tags t LEFT JOIN categories c ON c.id = t.category_id WHERE t.id = ?1",
        [tag_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or_else(|_| (String::new(), None))
}

pub(crate) fn remove_tags(conn: &mut Connection, ids: &[i64], tag_ids: &[i64]) -> Result<Value, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut st = tx
            .prepare("DELETE FROM media_tags WHERE media_id = ?1 AND tag_id = ?2")
            .map_err(|e| e.to_string())?;
        let labels: Vec<(String, Option<String>)> = tag_ids.iter().map(|t| tag_label(&tx, *t)).collect();
        for m in ids {
            for (t, (name, cat)) in tag_ids.iter().zip(&labels) {
                if st.execute(params![m, t]).map_err(|e| e.to_string())? > 0 {
                    crate::history::record(&tx, *m, "tag_remove", json!({ "tagId": t, "name": name, "category": cat }));
                }
            }
        }
    }
    touch(&tx, ids);
    tx.commit().map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, ids);
    log(conn, "tags.remove", ids.len(), &format!("{tag_ids:?}"));
    Ok(json!({ "updated": ids.len() }))
}

fn list_tags(conn: &Connection) -> Value {
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT t.id, t.name, c.id, c.name, c.color,
                    (SELECT COUNT(*) FROM media_tags mt WHERE mt.tag_id = t.id
                       AND EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = mt.media_id AND o.missing = 0))
             FROM tags t LEFT JOIN categories c ON c.id = t.category_id
             ORDER BY c.sort, c.name COLLATE NOCASE, t.name COLLATE NOCASE",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "categoryId": r.get::<_, Option<i64>>(2)?,
                    "category": r.get::<_, Option<String>>(3)?,
                    "color": r.get::<_, Option<String>>(4)?,
                    "count": r.get::<_, i64>(5)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

fn list_categories(conn: &Connection) -> Value {
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT c.id, c.name, c.color, c.sort, (SELECT COUNT(*) FROM tags t WHERE t.category_id = c.id)
             FROM categories c ORDER BY c.sort, c.name COLLATE NOCASE",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                    "color": r.get::<_, Option<String>>(2)?, "sort": r.get::<_, i64>(3)?,
                    "tagCount": r.get::<_, i64>(4)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

fn merge_tag(conn: &mut Connection, from: i64, into: i64) -> Result<Value, String> {
    if from == into {
        return Err("cannot merge a tag into itself".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let affected: Vec<i64> = {
        let mut st = tx.prepare("SELECT media_id FROM media_tags WHERE tag_id = ?1").map_err(|e| e.to_string())?;
        st.query_map([from], |r| r.get(0)).map_err(|e| e.to_string())?.flatten().collect()
    };
    tx.execute(
        "INSERT OR IGNORE INTO media_tags (media_id, tag_id) SELECT media_id, ?2 FROM media_tags WHERE tag_id = ?1",
        params![from, into],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM media_tags WHERE tag_id = ?1", [from]).map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM tags WHERE id = ?1", [from]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, &affected);
    log(conn, "tag.merge", affected.len(), &format!("{from} -> {into}"));
    Ok(json!({ "updated": affected.len() }))
}

fn tagged_media(conn: &Connection, tag_id: i64) -> Vec<i64> {
    conn.prepare("SELECT media_id FROM media_tags WHERE tag_id = ?1")
        .and_then(|mut st| st.query_map([tag_id], |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Albums + saved searches
// ---------------------------------------------------------------------------

fn list_albums(conn: &Connection) -> Value {
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT a.id, a.name, a.description, a.created_at,
                    (SELECT COUNT(*) FROM album_items ai WHERE ai.album_id = a.id),
                    (SELECT ai.media_id FROM album_items ai WHERE ai.album_id = a.id ORDER BY ai.added_at DESC LIMIT 1)
             FROM albums a ORDER BY a.name COLLATE NOCASE",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                    "description": r.get::<_, String>(2)?, "createdAt": r.get::<_, i64>(3)?,
                    "count": r.get::<_, i64>(4)?, "coverId": r.get::<_, Option<i64>>(5)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

fn list_searches(conn: &Connection) -> Value {
    let rows: Vec<Value> = conn
        .prepare("SELECT id, name, filter_json, created_at FROM saved_searches ORDER BY name COLLATE NOCASE")
        .and_then(|mut st| {
            st.query_map([], |r| {
                let f: String = r.get(2)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                    "filter": serde_json::from_str::<Value>(&f).unwrap_or(json!({})),
                    "createdAt": r.get::<_, i64>(3)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

// ---------------------------------------------------------------------------
// Catalog integrity: carry-over, duplicate groups, moved-path pruning
// ---------------------------------------------------------------------------

/// Copy notes/rating/favorite/color, tags and album membership from one media
/// row to another (used when a file is edited in place and its content hash
/// changes). Never overwrites annotations the target already has.
pub fn carry_annotations(conn: &Connection, from: i64, to: i64) {
    let _ = conn.execute(
        "INSERT OR IGNORE INTO annotations (media_id, notes, rating, favorite, color_label, updated_at)
         SELECT ?2, notes, rating, favorite, color_label, updated_at FROM annotations WHERE media_id = ?1",
        params![from, to],
    );
    let _ = conn.execute(
        "INSERT OR IGNORE INTO media_tags (media_id, tag_id) SELECT ?2, tag_id FROM media_tags WHERE media_id = ?1",
        params![from, to],
    );
    let _ = conn.execute(
        "INSERT OR IGNORE INTO album_items (album_id, media_id, added_at)
         SELECT album_id, ?2, added_at FROM album_items WHERE media_id = ?1",
        params![from, to],
    );
}

/// Settle queued content changes (see pending_carry in db.rs). Only entries at
/// least `min_age_secs` old are decided, so the live watcher gives a rename
/// shuffle time to finish; a full sync passes 0 after seeing every file.
///  * old content still on disk somewhere -> it moved; nothing to carry.
///  * old content gone                    -> edited in place: carry notes,
///    tags, rating, albums to the new version; drop the old row if it has no
///    path left at all.
pub fn resolve_pending_carry(conn: &mut Connection, min_age_secs: i64) -> usize {
    let cutoff = now() - min_age_secs;
    let rows: Vec<(i64, i64)> = conn
        .prepare("SELECT old_media, new_media FROM pending_carry WHERE at <= ?1 ORDER BY at")
        .and_then(|mut st| st.query_map([cutoff], |r| Ok((r.get(0)?, r.get(1)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    if rows.is_empty() {
        return 0;
    }
    let Ok(tx) = conn.transaction() else { return 0 };
    let mut carried = 0;
    let mut touched = vec![];
    for (old, new) in &rows {
        let present: i64 = tx
            .query_row("SELECT COUNT(*) FROM occurrence WHERE media_id = ?1 AND missing = 0", [old], |r| r.get(0))
            .unwrap_or(1);
        if present == 0 {
            carry_annotations(&tx, *old, *new);
            carried += 1;
            touched.push(*new);
            let any: i64 = tx
                .query_row("SELECT COUNT(*) FROM occurrence WHERE media_id = ?1", [old], |r| r.get(0))
                .unwrap_or(1);
            if any == 0 {
                delete_media_rows(&tx, *old);
            }
        }
        let _ = tx.execute(
            "DELETE FROM pending_carry WHERE old_media = ?1 AND new_media = ?2",
            params![old, new],
        );
    }
    let _ = tx.commit();
    crate::query::fts_refresh(conn, &touched);
    carried
}

/// Name-based fallback for a photo that was BOTH moved and edited (neither path
/// nor content hash matches any more). A new item inherits the notes, tags,
/// rating and albums of a vanished item only when ALL of these hold:
///   * same file name (case-insensitive),
///   * same date taken, read from EXIF or the file name (not file time),
///   * exactly one vanished candidate, and the new item has no notes/tags yet.
/// IMG_0001.jpg from two different cameras will not match: the dates differ.
pub fn reconcile_by_name(conn: &mut Connection, new_since_id: i64) -> usize {
    let gone: Vec<(i64, String, String)> = conn
        .prepare(
            "SELECT m.id, o.path, m.taken_at FROM media m JOIN occurrence o ON o.media_id = m.id
             WHERE m.deleted_at IS NULL AND m.date_source IN ('exif','filename') AND m.taken_at IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM occurrence x WHERE x.media_id = m.id AND x.missing = 0)
               AND (EXISTS (SELECT 1 FROM media_tags t WHERE t.media_id = m.id)
                 OR EXISTS (SELECT 1 FROM album_items al WHERE al.media_id = m.id)
                 OR EXISTS (SELECT 1 FROM annotations a WHERE a.media_id = m.id
                             AND (a.notes <> '' OR a.rating > 0 OR a.favorite = 1 OR a.color_label IS NOT NULL)))",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    if gone.is_empty() {
        return 0;
    }
    let fname = |p: &str| p.rsplit(['\\', '/']).next().unwrap_or(p).to_lowercase();
    let mut by_key: HashMap<(String, String), Vec<i64>> = HashMap::new();
    for (id, path, taken) in &gone {
        let e = by_key.entry((fname(path), taken.clone())).or_default();
        if !e.contains(id) {
            e.push(*id);
        }
    }
    let fresh: Vec<(i64, String, String)> = conn
        .prepare(
            "SELECT m.id, o.path, m.taken_at FROM media m JOIN occurrence o ON o.media_id = m.id AND o.missing = 0
             WHERE m.id > ?1 AND m.date_source IN ('exif','filename') AND m.taken_at IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM media_tags t WHERE t.media_id = m.id)
               AND NOT EXISTS (SELECT 1 FROM annotations a WHERE a.media_id = m.id)",
        )
        .and_then(|mut st| st.query_map([new_since_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    // Each vanished item may be claimed by exactly one new file.
    let mut claims: HashMap<i64, Vec<i64>> = HashMap::new();
    for (new_id, path, taken) in &fresh {
        if let Some(c) = by_key.get(&(fname(path), taken.clone())) {
            if c.len() == 1 {
                claims.entry(c[0]).or_default().push(*new_id);
            }
        }
    }
    let Ok(tx) = conn.transaction() else { return 0 };
    let mut n = 0;
    let mut touched = vec![];
    for (old, news) in &claims {
        if news.len() != 1 {
            continue; // ambiguous: two new files look like the same old one
        }
        carry_annotations(&tx, *old, news[0]);
        delete_media_rows(&tx, *old);
        touched.push(news[0]);
        n += 1;
    }
    let _ = tx.commit();
    if n > 0 {
        crate::query::fts_refresh(conn, &touched);
        log(conn, "reconcile.name", n, "notes/tags matched to moved+edited files by name and date taken");
    }
    n
}

/// Remove one media row and everything hanging off it (annotations, tags,
/// albums, its thumbnail file). Callers decide when that is safe.
pub fn delete_media_rows(conn: &Connection, id: i64) {
    let thumb: Option<String> = conn
        .query_row("SELECT thumb_path FROM media WHERE id = ?1", [id], |r| r.get(0))
        .ok()
        .flatten();
    for sql in [
        "DELETE FROM media_tags WHERE media_id = ?1",
        "DELETE FROM album_items WHERE media_id = ?1",
        "DELETE FROM annotations WHERE media_id = ?1",
        "DELETE FROM occurrence WHERE media_id = ?1",
        "DELETE FROM media WHERE id = ?1",
    ] {
        let _ = conn.execute(sql, [id]);
    }
    if let Some(t) = thumb {
        let _ = std::fs::remove_file(t);
    }
}

/// Rebuild duplicate groups from what is actually ON DISK. A group needs two or
/// more present copies; groups left with one copy (e.g. after the Takeout
/// copies were removed) are dissolved. An existing present canonical is kept;
/// otherwise the first configured root wins, then the shortest path.
pub fn recompute_dups(conn: &mut Connection, cfg: &Config) -> usize {
    let root_order: HashMap<String, usize> =
        cfg.roots.iter().enumerate().map(|(i, r)| (r.name.clone(), i)).collect();
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let _ = tx.execute(
        "UPDATE occurrence SET dup_group = NULL, is_canonical = 0
         WHERE (dup_group IS NOT NULL OR is_canonical = 1)
           AND (missing = 1 OR media_id IN (
                 SELECT media_id FROM occurrence WHERE missing = 0 GROUP BY media_id HAVING COUNT(*) < 2))",
        [],
    );
    let rows: Vec<(i64, i64, String, String, i64)> = tx
        .prepare(
            "SELECT media_id, id, path, root, is_canonical FROM occurrence
             WHERE missing = 0 AND media_id IN (
               SELECT media_id FROM occurrence WHERE missing = 0 GROUP BY media_id HAVING COUNT(*) >= 2)
             ORDER BY media_id, id",
        )
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    let mut groups: Vec<(i64, Vec<(i64, String, String, i64)>)> = vec![];
    for (mid, id, path, root, canon) in rows {
        if groups.last().map(|g| g.0) != Some(mid) {
            groups.push((mid, vec![]));
        }
        groups.last_mut().unwrap().1.push((id, path, root, canon));
    }
    for (mid, occ) in &groups {
        let chosen = occ.iter().find(|o| o.3 == 1).map(|o| o.0).unwrap_or_else(|| {
            let mut best = &occ[0];
            for c in occ {
                let bo = *root_order.get(&best.2).unwrap_or(&999);
                let co = *root_order.get(&c.2).unwrap_or(&999);
                if co < bo || (co == bo && c.1.len() < best.1.len()) {
                    best = c;
                }
            }
            best.0
        });
        let _ = tx.execute(
            "UPDATE occurrence SET dup_group = ?1, is_canonical = (id = ?2) WHERE media_id = ?1 AND missing = 0",
            params![mid, chosen],
        );
    }
    let _ = tx.commit();
    groups.len()
}

/// Drop stale path rows for content that still exists at another path (the
/// file was moved or a duplicate copy was removed). Annotations live on the
/// media row, so nothing the user entered is lost. Paths referenced by an
/// applied (undoable) journal entry are kept.
pub fn prune_moved(conn: &Connection) -> usize {
    conn.execute(
        "DELETE FROM occurrence
         WHERE missing = 1
           AND EXISTS (SELECT 1 FROM occurrence o2 WHERE o2.media_id = occurrence.media_id AND o2.missing = 0)
           AND NOT EXISTS (SELECT 1 FROM op_journal j WHERE j.occ_id = occurrence.id AND j.status = 'applied')",
        [],
    )
    .unwrap_or(0)
}

/// Remove catalog rows for content with NO copy left on disk, along with its
/// notes/tags/album membership. Explicit user action only.
fn purge_gone(conn: &mut Connection) -> Result<usize, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let gone: Vec<(i64, Option<String>)> = tx
        .prepare(
            "SELECT m.id, m.thumb_path FROM media m
             WHERE NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|it| it.flatten().collect()))
        .map_err(|e| e.to_string())?;
    for (id, _) in &gone {
        delete_media_rows(&tx, *id);
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(gone.len())
}

/// Full sync with disk: walk the roots, catalog new/changed files, mark gone
/// ones missing, prune moved paths, rebuild dup groups. Returns a summary.
pub fn sync(conn: &mut Connection, cfg: &Config) -> Value {
    sync_with_progress(conn, cfg, &|_| {})
}

/// Same as `sync`, reporting progress. Cleanup (missing / moved / duplicate
/// groups) runs FIRST — it takes milliseconds — so stale duplicate badges are
/// fixed immediately, not after hashing thousands of new files.
/// Re-attribute every path row to the library folder it lives under (folders
/// can be added, renamed or nested differently over time). Paths outside all
/// folders keep their old label. Returns how many rows changed.
pub fn fix_roots(conn: &mut Connection, cfg: &Config) -> usize {
    let rows: Vec<(i64, String, String)> = conn
        .prepare("SELECT id, path, root FROM occurrence")
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    let changes: Vec<(i64, String)> = rows
        .into_iter()
        .filter_map(|(id, path, root)| {
            let r = cfg.root_for_path(&path)?;
            (r.name != root).then(|| (id, r.name.clone()))
        })
        .collect();
    if changes.is_empty() {
        return 0;
    }
    if let Ok(tx) = conn.transaction() {
        for (id, root) in &changes {
            let _ = tx.execute("UPDATE occurrence SET root = ?2 WHERE id = ?1", params![id, root]);
        }
        let _ = tx.commit();
    }
    changes.len()
}

pub fn sync_with_progress(conn: &mut Connection, cfg: &Config, progress: &dyn Fn(Value)) -> Value {
    let t0 = std::time::Instant::now();
    fix_roots(conn, cfg);
    let max_id_before: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM media", [], |r| r.get(0)).unwrap_or(0);
    let diff = crate::incremental::diff_scan(conn, cfg);
    for p in &diff.missing_paths {
        crate::incremental::mark_missing(conn, p);
    }
    let mut pruned = prune_moved(conn);
    recompute_dups(conn, cfg);
    let total = diff.new_paths.len();
    progress(json!({ "phase": "cleaned", "done": 0, "total": total,
                     "missing": diff.missing_paths.len(), "pruned": pruned }));

    let mut added = 0usize;
    let mut failed: Vec<String> = vec![];
    for (i, p) in diff.new_paths.iter().enumerate() {
        match crate::incremental::process_file(conn, cfg, p) {
            Ok(_) => added += 1,
            Err(e) => failed.push(format!("{}: {e}", p.display())),
        }
        if (i + 1) % 250 == 0 && i + 1 < total {
            progress(json!({ "phase": "adding", "done": i + 1, "total": total }));
        }
    }
    resolve_pending_carry(conn, 0);
    let matched_by_name = if total > 0 { reconcile_by_name(conn, max_id_before) } else { 0 };
    if total > 0 {
        pruned += prune_moved(conn);
    }
    let groups = recompute_dups(conn, cfg);
    crate::query::ensure_fts(conn);
    if added > 0 || !diff.missing_paths.is_empty() || pruned > 0 {
        crate::query::force_rebuild_fts(conn);
        log(
            conn,
            "sync",
            added + diff.missing_paths.len(),
            &format!("{added} added, {} missing, {pruned} moved-path rows pruned", diff.missing_paths.len()),
        );
    }
    json!({
        "added": added,
        "missing": diff.missing_paths.len(),
        "pruned": pruned,
        "dupGroups": groups,
        "matchedByName": matched_by_name,
        "failed": failed.iter().take(50).collect::<Vec<_>>(),
        "failedCount": failed.len(),
        "unreachableRoots": diff.unreachable_roots,
        "seconds": t0.elapsed().as_secs_f64(),
    })
}

fn maintenance_summary(conn: &Connection, cfg: &Config) -> Value {
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let roots: Vec<Value> = cfg
        .all_roots()
        .iter()
        .map(|r| json!({ "name": r.name, "path": r.path, "online": Path::new(&r.path).is_dir(),
                         "enabled": r.enabled, "protected": r.protected }))
        .collect();
    let last_backup: Option<String> = conn
        .query_row("SELECT value FROM app_meta WHERE key = 'last_backup'", [], |r| r.get(0))
        .ok();
    json!({
        "roots": roots,
        "media": q("SELECT COUNT(*) FROM media"),
        "present": q("SELECT COUNT(*) FROM media m WHERE EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)"),
        "gone": q("SELECT COUNT(*) FROM media m WHERE NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)"),
        "goneAnnotated": q("SELECT COUNT(*) FROM media m WHERE NOT EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)
                             AND (EXISTS (SELECT 1 FROM media_tags t WHERE t.media_id = m.id)
                               OR EXISTS (SELECT 1 FROM annotations a WHERE a.media_id = m.id AND (a.notes <> '' OR a.rating > 0 OR a.favorite = 1)))"),
        "movedPaths": q("SELECT COUNT(*) FROM occurrence o WHERE o.missing = 1 AND EXISTS (SELECT 1 FROM occurrence o2 WHERE o2.media_id = o.media_id AND o2.missing = 0)"),
        "dupGroups": q("SELECT COUNT(DISTINCT dup_group) FROM occurrence WHERE dup_group IS NOT NULL AND missing = 0"),
        "noThumb": q("SELECT COUNT(*) FROM media WHERE thumb_path IS NULL"),
        "tagged": q("SELECT COUNT(DISTINCT media_id) FROM media_tags"),
        "withNotes": q("SELECT COUNT(*) FROM annotations WHERE notes <> ''"),
        "tags": q("SELECT COUNT(*) FROM tags"),
        "albums": q("SELECT COUNT(*) FROM albums"),
        "lastBackup": last_backup,
        "dbPath": crate::paths::db_path(),
    })
}

// ---------------------------------------------------------------------------
// Backup + export
// ---------------------------------------------------------------------------

fn stamp() -> String {
    // yyyymmdd-hhmmss (UTC), from the scanner's civil-date helper.
    let t = now();
    let d = crate::scanner::epoch_to_date(t); // "YYYY-MM-DD HH:MM:00"
    format!("{}{}{}-{}{}{:02}", &d[0..4], &d[5..7], &d[8..10], &d[11..13], &d[14..16], t % 60)
}

/// Consistent snapshot of the catalog (VACUUM INTO), keeping the newest 20.
pub fn backup(conn: &Connection) -> Result<String, String> {
    let dir = crate::paths::data_dir().join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("catalog-{}.db", stamp()));
    if out.exists() {
        return Ok(out.to_string_lossy().to_string());
    }
    conn.execute("VACUUM INTO ?1", [out.to_string_lossy().to_string()])
        .map_err(|e| e.to_string())?;
    let _ = conn.execute(
        "INSERT OR REPLACE INTO app_meta(key, value) VALUES ('last_backup', ?1)",
        [now().to_string()],
    );
    // Retention.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("catalog-")).unwrap_or(false))
            .collect();
        files.sort();
        while files.len() > 20 {
            let _ = std::fs::remove_file(files.remove(0));
        }
    }
    Ok(out.to_string_lossy().to_string())
}

/// Daily safety snapshot, called at app launch (cheap no-op if one exists today).
pub fn backup_if_due(conn: &Connection) {
    let last: i64 = conn
        .query_row("SELECT value FROM app_meta WHERE key = 'last_backup'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if now() - last > 20 * 3600 {
        if let Err(e) = backup(conn) {
            eprintln!("backup failed: {e}");
        }
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn export_csv(conn: &Connection, filter: &crate::query::Filter) -> Result<Value, String> {
    let items = crate::query::apply_filter(conn, filter)?;
    let dir = crate::paths::data_dir().join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(format!("export-{}.csv", stamp()));
    let mut s = String::from("\u{feff}id,path,taken_at,kind,width,height,bytes,camera,rating,favorite,color_label,tags,albums,notes\r\n");
    let mut notes_st = conn
        .prepare("SELECT notes FROM annotations WHERE media_id = ?1")
        .map_err(|e| e.to_string())?;
    let mut alb_st = conn
        .prepare("SELECT group_concat(a.name, '; ') FROM album_items ai JOIN albums a ON a.id = ai.album_id WHERE ai.media_id = ?1")
        .map_err(|e| e.to_string())?;
    for it in &items {
        let notes: String = notes_st.query_row([it.id], |r| r.get(0)).unwrap_or_default();
        let albums: Option<String> = alb_st.query_row([it.id], |r| r.get(0)).unwrap_or(None);
        let row = [
            it.id.to_string(),
            it.path.clone(),
            it.taken_at.clone().unwrap_or_default(),
            it.kind.clone(),
            it.width.map(|v| v.to_string()).unwrap_or_default(),
            it.height.map(|v| v.to_string()).unwrap_or_default(),
            it.bytes.to_string(),
            it.camera.clone().unwrap_or_default(),
            it.rating.to_string(),
            if it.favorite { "yes".into() } else { String::new() },
            it.color_label.clone().unwrap_or_default(),
            it.tags.clone().unwrap_or_default(),
            albums.unwrap_or_default(),
            notes,
        ];
        s.push_str(&row.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        s.push_str("\r\n");
    }
    std::fs::write(&out, s).map_err(|e| e.to_string())?;
    log(conn, "export.csv", items.len(), &out.to_string_lossy());
    Ok(json!({ "path": out.to_string_lossy(), "rows": items.len() }))
}

// ---------------------------------------------------------------------------
// Folders + bulk move
// ---------------------------------------------------------------------------

fn parent_dir(p: &str) -> String {
    let i = p.rfind(['\\', '/']).unwrap_or(0);
    p[..i].to_string()
}

/// Every folder that currently holds media, with counts (move-target picker).
fn list_folders(conn: &Connection) -> Value {
    let mut counts: HashMap<String, (String, i64)> = HashMap::new();
    if let Ok(mut st) = conn.prepare("SELECT path FROM occurrence WHERE missing = 0") {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) {
            for p in rows.flatten() {
                let d = parent_dir(&p);
                let e = counts.entry(crate::paths::key(&d)).or_insert((d, 0));
                e.1 += 1;
            }
        }
    }
    let mut v: Vec<(String, i64)> = counts.into_values().collect();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    Value::Array(v.into_iter().map(|(p, n)| json!({ "path": p, "count": n })).collect())
}

fn under_a_root(cfg: &Config, dir: &str) -> bool {
    let k = crate::paths::key(dir);
    cfg.roots.iter().any(|r| {
        let rk = crate::paths::key(&r.path);
        let rk = rk.trim_end_matches('\\');
        k == rk || k.starts_with(&format!("{rk}\\"))
    })
}

fn sep_for(dir: &str) -> &'static str {
    if dir.contains('\\') || cfg!(windows) { "\\" } else { "/" }
}

/// Plan (dry-run) or apply a move of many items into one folder. Uses the
/// existing per-item relocate engine, so every move is journaled + undoable and
/// collisions fail instead of overwriting.
fn bulk_move(conn: &mut Connection, cfg: &Config, ids: &[i64], dir: &str, apply: bool) -> Result<Value, String> {
    let dir = crate::paths::canon(dir.trim().trim_end_matches(['\\', '/']));
    if dir.is_empty() {
        return Err("choose a destination folder".into());
    }
    if !under_a_root(cfg, &dir) {
        return Err(format!("destination must be inside a library root ({})",
            cfg.roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>().join(", ")));
    }
    let sep = sep_for(&dir);
    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut results = vec![];
    let (mut ok, mut failed, mut skipped) = (0, 0, 0);
    for id in ids {
        let cur: Option<String> = conn
            .query_row(
                "SELECT path FROM occurrence WHERE media_id = ?1
                 ORDER BY CASE WHEN is_canonical = 1 THEN 0 ELSE 1 END, missing, id LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .ok();
        let Some(cur) = cur else {
            failed += 1;
            results.push(json!({ "id": id, "ok": false, "error": "not in catalog" }));
            continue;
        };
        if cfg.root_for_path(&cur).map(|r| r.protected).unwrap_or(false) {
            failed += 1;
            results.push(json!({ "id": id, "ok": false, "from": cur, "error": "in a protected folder (Settings)" }));
            continue;
        }
        let fname = cur.rsplit(['\\', '/']).next().unwrap_or(&cur).to_string();
        let target = format!("{dir}{sep}{fname}");
        if crate::paths::key(&parent_dir(&cur)) == crate::paths::key(&dir) {
            skipped += 1;
            results.push(json!({ "id": id, "ok": true, "skipped": true, "from": cur, "to": target }));
            continue;
        }
        if !seen.insert(crate::paths::key(&target)) {
            failed += 1;
            results.push(json!({ "id": id, "ok": false, "from": cur, "to": target,
                                 "error": "another selected file has the same name" }));
            continue;
        }
        let r = if apply {
            crate::ops::apply_relocate(conn, *id, &target, "move")
        } else {
            crate::ops::plan_relocate(conn, *id, &target, "move")
        };
        match r {
            Ok(p) => {
                ok += 1;
                results.push(json!({ "id": id, "ok": true, "from": p.original_path, "to": p.target_path,
                                     "warnings": p.warnings }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({ "id": id, "ok": false, "from": cur, "to": target, "error": e }));
            }
        }
    }
    if apply {
        log(conn, "bulk.move", ok, &format!("{ok} moved to {dir}, {failed} failed"));
    }
    Ok(json!({ "dir": dir, "ok": ok, "failed": failed, "skipped": skipped, "applied": apply, "results": results }))
}

fn bulk_undo(conn: &mut Connection, ids: &[i64]) -> Value {
    let mut ok = 0;
    let mut errors = vec![];
    for id in ids {
        match crate::ops::undo_relocate(conn, *id) {
            Ok(p) => {
                ok += 1;
                errors.extend(p.warnings);
            }
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    log(conn, "bulk.undo", ok, "");
    json!({ "undone": ok, "errors": errors })
}

// ---------------------------------------------------------------------------
// Copy to… (export copies anywhere; the library itself is untouched)
// ---------------------------------------------------------------------------

/// True when two files hold the same bytes (size check first, then a stream compare).
fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(fa), Ok(fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
    let (mut ra, mut rb) = (std::io::BufReader::new(fa), std::io::BufReader::new(fb));
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = match ra.read(&mut ba) { Ok(n) => n, Err(_) => return false };
        if na == 0 {
            return rb.read(&mut bb).map(|n| n == 0).unwrap_or(false);
        }
        if rb.read_exact(&mut bb[..na]).is_err() || ba[..na] != bb[..na] {
            return false;
        }
    }
}

/// Where to copy `src` in `dir`: walks `name`, `stem (2).ext`, `(3)`…
/// Ok(free path) = first unused name; Err(path) = that name already holds
/// identical content (nothing to copy).
fn copy_target(dir: &Path, fname: &str, src: &Path) -> Result<PathBuf, PathBuf> {
    let (stem, ext) = match fname.rfind('.') {
        Some(i) if i > 0 => (&fname[..i], &fname[i..]),
        _ => (fname, ""),
    };
    for n in 1.. {
        let p = if n == 1 { dir.join(fname) } else { dir.join(format!("{stem} ({n}){ext}")) };
        if !p.exists() {
            return Ok(p);
        }
        if same_content(src, &p) {
            return Err(p);
        }
    }
    unreachable!()
}

/// Copy the selected items' files into `dir` (any folder, inside the library or
/// not). Never overwrites: an identical file already there is skipped, a
/// different file with the same name gets " (2)" etc. The originals, the
/// catalog rows and the annotations are not touched.
fn copy_to(conn: &Connection, ids: &[i64], dir: &str) -> Result<Value, String> {
    let dir_s = dir.trim().trim_end_matches(['\\', '/']).to_string();
    if dir_s.is_empty() {
        return Err("choose a destination folder".into());
    }
    let dest = PathBuf::from(if dir_s.ends_with(':') { format!("{dir_s}\\") } else { dir_s.clone() });
    if !dest.is_dir() {
        return Err(format!("destination folder not found: {dir_s}"));
    }
    let (mut copied, mut renamed, mut same, mut failed, mut bytes) = (0usize, 0usize, 0usize, 0usize, 0u64);
    let mut results = vec![];
    for id in ids {
        // First copy that is actually on disk (canonical preferred).
        let paths: Vec<String> = conn
            .prepare(
                "SELECT path FROM occurrence WHERE media_id = ?1 AND missing = 0
                 ORDER BY CASE WHEN is_canonical = 1 THEN 0 ELSE 1 END, id",
            )
            .and_then(|mut st| st.query_map([id], |r| r.get(0)).map(|it| it.flatten().collect()))
            .unwrap_or_default();
        let Some(src) = paths.iter().map(PathBuf::from).find(|p| p.is_file()) else {
            failed += 1;
            results.push(json!({ "id": id, "ok": false, "error": "no copy on disk" }));
            continue;
        };
        let fname = src.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        let target = match copy_target(&dest, &fname, &src) {
            Ok(t) => t,
            Err(existing) => {
                same += 1;
                results.push(json!({ "id": id, "ok": true, "skipped": true, "from": src, "to": existing }));
                continue;
            }
        };
        match std::fs::copy(&src, &target) {
            Ok(n) => {
                // Keep the original's modified date (Windows already does; this
                // covers other platforms).
                if let Ok(m) = std::fs::metadata(&src).and_then(|m| m.modified()) {
                    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&target) {
                        let _ = f.set_modified(m);
                    }
                }
                copied += 1;
                bytes += n;
                if target.file_name().map(|f| f.to_string_lossy() != fname.as_str()).unwrap_or(false) {
                    renamed += 1;
                }
                results.push(json!({ "id": id, "ok": true, "from": src, "to": target }));
            }
            Err(e) => {
                // `target` was a free name, so anything there now is our own partial copy.
                let _ = std::fs::remove_file(&target);
                failed += 1;
                results.push(json!({ "id": id, "ok": false, "from": src, "to": target, "error": e.to_string() }));
            }
        }
    }
    let dir_out = dest.to_string_lossy().to_string();
    log(conn, "copy", copied, &format!("{copied} copied to {dir_out}, {same} already there, {failed} failed"));
    Ok(json!({ "dir": dir_out, "copied": copied, "renamed": renamed, "alreadyThere": same,
               "failed": failed, "bytes": bytes, "results": results }))
}

/// Native "choose folder" dialog, opened in `start` when that folder exists.
/// The Tauri command adds the app window as parent so the dialog is modal.
pub fn folder_dialog(start: Option<&str>, title: &str) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new().set_title(title);
    if let Some(s) = start.filter(|s| !s.is_empty() && Path::new(s).is_dir()) {
        d = d.set_directory(s);
    }
    d
}

/// Show the dialog: { path } or { path: null } when cancelled.
pub fn pick_folder(d: rfd::FileDialog) -> Value {
    json!({ "path": d.pick_folder().map(|p| crate::paths::canon(&p.to_string_lossy())) })
}

// ---------------------------------------------------------------------------
// Delete (Recycle Bin) + restore
// ---------------------------------------------------------------------------

/// Roots whose drive was verified to have a working Recycle Bin this session.
static RECYCLE_OK: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Prove the drive holding `root` really recycles: trash a tiny probe file,
/// find it in the bin, then purge it. On drives without a Recycle Bin (many
/// USB / network drives) Windows would destroy files instead, so we refuse.
pub(crate) fn ensure_recycle_bin(root: &str) -> Result<(), String> {
    let k = crate::paths::key(root);
    if RECYCLE_OK.lock().map(|v| v.contains(&k)).unwrap_or(false) {
        return Ok(());
    }
    let probe = Path::new(root).join(format!(".photoapp-recycle-probe-{}-{}.tmp", now(), std::process::id()));
    std::fs::write(&probe, b"photoApp recycle bin check - safe to delete")
        .map_err(|e| format!("cannot write to {root}: {e}"))?;
    if let Err(e) = trash::delete(&probe) {
        let _ = std::fs::remove_file(&probe);
        return Err(format!("the Recycle Bin is not available on {root}: {e}"));
    }
    let name = probe.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    let found: Vec<trash::TrashItem> = trash::os_limited::list()
        .map_err(|e| format!("cannot read the Recycle Bin: {e}"))?
        .into_iter()
        .filter(|it| it.name == name)
        .collect();
    if found.is_empty() {
        return Err(format!(
            "{root} has no Recycle Bin (files there would be erased permanently), so photoApp will not delete from it"
        ));
    }
    let _ = trash::os_limited::purge_all(found);
    if let Ok(mut v) = RECYCLE_OK.lock() {
        v.push(k);
    }
    Ok(())
}

fn root_of<'a>(cfg: &'a Config, path: &str) -> Option<&'a crate::scanner::Root> {
    let k = crate::paths::key(path);
    cfg.roots
        .iter()
        .filter(|r| {
            let rk = crate::paths::key(&r.path);
            let rk = rk.trim_end_matches('\\');
            k.starts_with(&format!("{rk}\\"))
        })
        .max_by_key(|r| r.path.len())
}

/// Send every on-disk copy of the given items to the Recycle Bin. Notes, tags,
/// ratings and albums are kept (item marked deleted) so a restore brings the
/// photo back exactly as it was.
fn delete_items(conn: &mut Connection, cfg: &Config, ids: &[i64]) -> Result<Value, String> {
    // Collect paths per item.
    let mut paths: Vec<(i64, i64, String)> = vec![]; // (media, occ, path)
    {
        let mut st = conn
            .prepare("SELECT id, path FROM occurrence WHERE media_id = ?1 AND missing = 0")
            .map_err(|e| e.to_string())?;
        for id in ids {
            let rows: Vec<(i64, String)> = st
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            for (occ, p) in rows {
                paths.push((*id, occ, p));
            }
        }
    }
    if paths.is_empty() {
        return Err("nothing to delete — the selected items are not on disk".into());
    }
    // Protected folders (Settings) are never deleted from.
    let mut protected_skipped: Vec<String> = vec![];
    paths.retain(|(_, _, p)| {
        let keep = !cfg.root_for_path(p).map(|r| r.protected).unwrap_or(false);
        if !keep {
            protected_skipped.push(format!("{p}: kept — it is in a protected folder"));
        }
        keep
    });
    if paths.is_empty() {
        return Err("these items are only in protected folders — unprotect the folder in Settings to delete them".into());
    }
    // Every file must sit under a library root whose drive recycles.
    let mut roots_checked: Vec<String> = vec![];
    for (_, _, p) in &paths {
        let root = root_of(cfg, p).ok_or_else(|| format!("{p} is outside the library roots"))?;
        if !roots_checked.contains(&root.path) {
            ensure_recycle_bin(&root.path)?;
            roots_checked.push(root.path.clone());
        }
    }

    let mut deleted_paths = 0usize;
    let mut failed: Vec<String> = protected_skipped;
    let mut gone_occ: Vec<i64> = vec![];
    for (_, occ, p) in &paths {
        if !Path::new(p).exists() {
            gone_occ.push(*occ); // already gone; just catch the catalog up
            continue;
        }
        match trash::delete(p) {
            Ok(()) if !Path::new(p).exists() => {
                deleted_paths += 1;
                gone_occ.push(*occ);
            }
            Ok(()) => failed.push(format!("{p}: still on disk after delete")),
            Err(e) => failed.push(format!("{p}: {e}")),
        }
    }

    let t = now();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for occ in &gone_occ {
        tx.execute("UPDATE occurrence SET missing = 1, indexed_at = ?2 WHERE id = ?1", params![occ, t])
            .map_err(|e| e.to_string())?;
    }
    let mut items_deleted = 0usize;
    for id in ids {
        let left: i64 = tx
            .query_row("SELECT COUNT(*) FROM occurrence WHERE media_id = ?1 AND missing = 0", [id], |r| r.get(0))
            .unwrap_or(1);
        if left == 0 {
            tx.execute("UPDATE media SET deleted_at = ?2 WHERE id = ?1", params![id, t])
                .map_err(|e| e.to_string())?;
            items_deleted += 1;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    recompute_dups(conn, cfg);
    log(
        conn,
        "delete",
        items_deleted,
        &format!("{deleted_paths} file(s) sent to the Recycle Bin{}", if failed.is_empty() { String::new() } else { format!(", {} failed", failed.len()) }),
    );
    Ok(json!({ "items": items_deleted, "files": deleted_paths, "failed": failed }))
}

/// Bring deleted items back from the Recycle Bin to their original paths and
/// re-catalog them (same content -> same item, notes and tags intact).
fn restore_items(conn: &mut Connection, cfg: &Config, ids: &[i64]) -> Result<Value, String> {
    let mut wanted: HashMap<String, i64> = HashMap::new();
    {
        let mut st = conn
            .prepare("SELECT path FROM occurrence WHERE media_id = ?1 AND missing = 1")
            .map_err(|e| e.to_string())?;
        for id in ids {
            for p in st.query_map([id], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?.flatten() {
                wanted.insert(crate::paths::key(&p), *id);
            }
        }
    }
    if wanted.is_empty() {
        return Err("no deleted files recorded for the selected items".into());
    }
    let all = trash::os_limited::list().map_err(|e| format!("cannot read the Recycle Bin: {e}"))?;
    // Newest bin entry per original path (the same path may have been deleted twice).
    let mut pick: HashMap<String, trash::TrashItem> = HashMap::new();
    for it in all {
        let k = crate::paths::key(&it.original_path().to_string_lossy());
        if !wanted.contains_key(&k) {
            continue;
        }
        let newer = pick.get(&k).map(|o| it.time_deleted > o.time_deleted).unwrap_or(true);
        if newer {
            pick.insert(k, it);
        }
    }
    let mut skipped: Vec<String> = vec![];
    let mut to_restore: Vec<trash::TrashItem> = vec![];
    for (k, it) in pick {
        if it.original_path().exists() {
            skipped.push(format!("{}: a file with that name exists again — not overwriting", it.original_path().display()));
        } else {
            to_restore.push(it);
        }
        let _ = k;
    }
    let restored_paths: Vec<PathBuf> = to_restore.iter().map(|i| i.original_path()).collect();
    if !to_restore.is_empty() {
        trash::os_limited::restore_all(to_restore).map_err(|e| format!("restore failed: {e}"))?;
    }
    let mut back = 0usize;
    for p in &restored_paths {
        if p.exists() && crate::incremental::process_file(conn, cfg, p).is_ok() {
            back += 1;
        }
    }
    let not_found = wanted.len().saturating_sub(back + skipped.len());
    recompute_dups(conn, cfg);
    crate::query::force_rebuild_fts(conn);
    log(conn, "restore", back, &format!("{back} file(s) restored from the Recycle Bin"));
    Ok(json!({ "files": back, "skipped": skipped, "notInBin": not_found }))
}

// ---------------------------------------------------------------------------
// Shell helpers (Windows Explorer)
// ---------------------------------------------------------------------------

fn reveal(path: &str, open: bool) -> Result<Value, String> {
    let p = crate::paths::canon(path);
    if !Path::new(&p).exists() {
        return Err(format!("file not found: {p}"));
    }
    #[cfg(windows)]
    {
        // raw_arg: explorer wants  /select,"E:\Photos\Smith Marine\x.jpg"  verbatim;
        // std's default quoting of the whole argument breaks on spaces.
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("explorer.exe");
        if open {
            c.raw_arg(format!("\"{p}\""));
        } else {
            c.raw_arg(format!("/select,\"{p}\""));
        }
        c.spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        let _ = open;
    }
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn call(conn: &mut Connection, cfg: &Config, cmd: &str, a: &Value) -> Result<Value, String> {
    match cmd {
        // --- annotations ---
        "annotation.get" => Ok(annotation(conn, int(a, "id")?)),
        "notes.set" => set_notes(conn, int(a, "id")?, &text(a, "notes")?),
        "notes.append" => append_notes(conn, &ids(a)?, &text(a, "text")?),
        "rating.set" => {
            let r = int(a, "rating")?;
            if !(0..=5).contains(&r) {
                return Err("rating must be 0-5".into());
            }
            set_field(conn, &ids(a)?, "rating", r.into())
        }
        "favorite.set" => {
            let v = a.get("value").and_then(|v| v.as_bool()).ok_or("missing value")?;
            set_field(conn, &ids(a)?, "favorite", (v as i64).into())
        }
        "color.set" => {
            let c = a.get("color").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            if let Some(c) = c {
                if !COLORS.contains(&c) {
                    return Err(format!("unknown color label {c}"));
                }
            }
            let v: rusqlite::types::Value = match c {
                Some(c) => c.to_string().into(),
                None => rusqlite::types::Value::Null,
            };
            set_field(conn, &ids(a)?, "color_label", v)
        }

        // --- tags + categories ---
        "tags.list" => Ok(list_tags(conn)),
        "tags.forItems" => Ok(Value::Array(item_tags(conn, &ids(a)?))),
        "tags.add" => {
            let names: Vec<String> = a
                .get("names")
                .and_then(|v| v.as_array())
                .ok_or("missing names")?
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect();
            add_tags(conn, &ids(a)?, &names, opt_int(a, "categoryId"))
        }
        "tags.remove" => {
            let t: Vec<i64> = a
                .get("tagIds")
                .and_then(|v| v.as_array())
                .ok_or("missing tagIds")?
                .iter()
                .filter_map(|x| x.as_i64())
                .collect();
            remove_tags(conn, &ids(a)?, &t)
        }
        "tag.create" => {
            let id = ensure_tag(conn, &name(a, "name")?, opt_int(a, "categoryId"))?;
            log(conn, "tag.create", 0, &name(a, "name")?);
            Ok(json!({ "id": id }))
        }
        "tag.rename" => {
            let id = int(a, "id")?;
            let n = name(a, "name")?;
            let clash: Option<i64> = conn
                .query_row("SELECT id FROM tags WHERE name = ?1 AND id <> ?2", params![n, id], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(other) = clash {
                return Err(format!("a tag named \"{n}\" already exists (id {other}) — use Merge instead"));
            }
            conn.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, n]).map_err(|e| e.to_string())?;
            let m = tagged_media(conn, id);
            crate::query::fts_refresh(conn, &m);
            log(conn, "tag.rename", m.len(), &n);
            Ok(json!({ "ok": true }))
        }
        "tag.setCategory" => {
            conn.execute(
                "UPDATE tags SET category_id = ?2 WHERE id = ?1",
                params![int(a, "id")?, opt_int(a, "categoryId")],
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "tag.delete" => {
            let id = int(a, "id")?;
            let m = tagged_media(conn, id);
            conn.execute("DELETE FROM media_tags WHERE tag_id = ?1", [id]).map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM tags WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
            crate::query::fts_refresh(conn, &m);
            log(conn, "tag.delete", m.len(), &id.to_string());
            Ok(json!({ "ok": true, "untagged": m.len() }))
        }
        "tag.merge" => merge_tag(conn, int(a, "fromId")?, int(a, "intoId")?),
        "categories.list" => Ok(list_categories(conn)),
        "category.create" => {
            let n = name(a, "name")?;
            let id = ensure_category(conn, &n)?;
            if let Some(c) = a.get("color").and_then(|v| v.as_str()) {
                conn.execute("UPDATE categories SET color = ?2 WHERE id = ?1", params![id, c]).ok();
            }
            Ok(json!({ "id": id }))
        }
        "category.update" => {
            let id = int(a, "id")?;
            if a.get("name").is_some() {
                conn.execute("UPDATE categories SET name = ?2 WHERE id = ?1", params![id, name(a, "name")?])
                    .map_err(|e| e.to_string())?;
            }
            if let Some(c) = a.get("color") {
                conn.execute("UPDATE categories SET color = ?2 WHERE id = ?1", params![id, c.as_str()])
                    .map_err(|e| e.to_string())?;
            }
            if let Some(s) = opt_int(a, "sort") {
                conn.execute("UPDATE categories SET sort = ?2 WHERE id = ?1", params![id, s]).map_err(|e| e.to_string())?;
            }
            Ok(json!({ "ok": true }))
        }
        "category.delete" => {
            let id = int(a, "id")?;
            conn.execute("UPDATE tags SET category_id = NULL WHERE category_id = ?1", [id]).map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM categories WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }

        // --- albums ---
        "albums.list" => Ok(list_albums(conn)),
        "album.create" => {
            let n = name(a, "name")?;
            let d = a.get("description").and_then(|v| v.as_str()).unwrap_or("");
            conn.execute(
                "INSERT INTO albums (name, description, created_at) VALUES (?1, ?2, ?3)",
                params![n, d, now()],
            )
            .map_err(|e| if e.to_string().contains("UNIQUE") { format!("album \"{n}\" already exists") } else { e.to_string() })?;
            let id = conn.last_insert_rowid();
            if let Ok(list) = ids(a) {
                add_to_album(conn, id, &list)?;
            }
            log(conn, "album.create", 0, &n);
            Ok(json!({ "id": id }))
        }
        "album.update" => {
            let id = int(a, "id")?;
            if a.get("name").is_some() {
                conn.execute("UPDATE albums SET name = ?2 WHERE id = ?1", params![id, name(a, "name")?])
                    .map_err(|e| e.to_string())?;
            }
            if let Some(d) = a.get("description").and_then(|v| v.as_str()) {
                conn.execute("UPDATE albums SET description = ?2 WHERE id = ?1", params![id, d])
                    .map_err(|e| e.to_string())?;
            }
            let m: Vec<i64> = conn
                .prepare("SELECT media_id FROM album_items WHERE album_id = ?1")
                .and_then(|mut st| st.query_map([id], |r| r.get(0)).map(|it| it.flatten().collect()))
                .unwrap_or_default();
            crate::query::fts_refresh(conn, &m);
            Ok(json!({ "ok": true }))
        }
        "album.delete" => {
            let id = int(a, "id")?;
            let m: Vec<i64> = conn
                .prepare("SELECT media_id FROM album_items WHERE album_id = ?1")
                .and_then(|mut st| st.query_map([id], |r| r.get(0)).map(|it| it.flatten().collect()))
                .unwrap_or_default();
            conn.execute("DELETE FROM album_items WHERE album_id = ?1", [id]).map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM albums WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
            crate::query::fts_refresh(conn, &m);
            log(conn, "album.delete", m.len(), &id.to_string());
            Ok(json!({ "ok": true }))
        }
        "album.add" => add_to_album(conn, int(a, "albumId")?, &ids(a)?),
        "album.remove" => {
            let album = int(a, "albumId")?;
            let list = ids(a)?;
            let aname: String = conn.query_row("SELECT name FROM albums WHERE id = ?1", [album], |r| r.get(0)).unwrap_or_default();
            for m in &list {
                let n = conn
                    .execute("DELETE FROM album_items WHERE album_id = ?1 AND media_id = ?2", params![album, m])
                    .map_err(|e| e.to_string())?;
                if n > 0 {
                    crate::history::record(conn, *m, "album_remove", json!({ "albumId": album, "name": aname }));
                }
            }
            crate::query::fts_refresh(conn, &list);
            log(conn, "album.remove", list.len(), &album.to_string());
            Ok(json!({ "updated": list.len() }))
        }

        // --- saved searches ---
        "searches.list" => Ok(list_searches(conn)),
        "search.save" => {
            let n = name(a, "name")?;
            let f = a.get("filter").cloned().unwrap_or(json!({}));
            conn.execute(
                "INSERT INTO saved_searches (name, filter_json, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET filter_json = excluded.filter_json",
                params![n, f.to_string(), now()],
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "search.delete" => {
            conn.execute("DELETE FROM saved_searches WHERE id = ?1", [int(a, "id")?]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }

        // --- maintenance ---
        "maintenance.summary" => Ok(maintenance_summary(conn, cfg)),
        "maintenance.sync" => Ok(sync(conn, cfg)),
        "maintenance.pruneMoved" => {
            let n = prune_moved(conn);
            recompute_dups(conn, cfg);
            crate::query::force_rebuild_fts(conn);
            log(conn, "cleanup.paths", n, "");
            Ok(json!({ "removed": n }))
        }
        "maintenance.purgeGone" => {
            backup(conn)?; // always snapshot before removing catalog rows
            let n = purge_gone(conn)?;
            crate::query::force_rebuild_fts(conn);
            log(conn, "cleanup.items", n, "");
            Ok(json!({ "removed": n }))
        }
        "maintenance.backup" => Ok(json!({ "path": backup(conn)? })),
        "activity.list" => {
            let lim = opt_int(a, "limit").unwrap_or(200).clamp(1, 2000);
            let rows: Vec<Value> = conn
                .prepare("SELECT at, action, media_count, detail FROM activity_log ORDER BY id DESC LIMIT ?1")
                .and_then(|mut st| {
                    st.query_map([lim], |r| {
                        Ok(json!({ "at": r.get::<_, i64>(0)?, "action": r.get::<_, String>(1)?,
                                   "count": r.get::<_, i64>(2)?, "detail": r.get::<_, String>(3)? }))
                    })
                    .map(|it| it.flatten().collect())
                })
                .unwrap_or_default();
            Ok(Value::Array(rows))
        }
        "export.csv" => {
            let f: crate::query::Filter = serde_json::from_value(a.get("filter").cloned().unwrap_or(json!({})))
                .map_err(|e| format!("bad filter: {e}"))?;
            export_csv(conn, &f)
        }

        // --- query helpers ---
        "query.ids" => {
            // All ids matching a filter (for "select all N results").
            let f: crate::query::Filter = serde_json::from_value(a.get("filter").cloned().unwrap_or(json!({})))
                .map_err(|e| format!("bad filter: {e}"))?;
            let items = crate::query::apply_filter(conn, &f)?;
            Ok(json!(items.iter().map(|i| i.id).collect::<Vec<_>>()))
        }

        // --- files ---
        "folders.list" => Ok(list_folders(conn)),
        "bulk.movePlan" => bulk_move(conn, cfg, &ids(a)?, &text(a, "dir")?, false),
        "bulk.moveApply" => bulk_move(conn, cfg, &ids(a)?, &text(a, "dir")?, true),
        "bulk.undo" => Ok(bulk_undo(conn, &ids(a)?)),
        "items.copyTo" => copy_to(conn, &ids(a)?, &text(a, "dir")?),
        "pickFolder" => Ok(pick_folder(folder_dialog(
            a.get("start").and_then(|v| v.as_str()),
            a.get("title").and_then(|v| v.as_str()).unwrap_or("Choose a folder"),
        ))),
        "items.delete" => delete_items(conn, cfg, &ids(a)?),
        "items.restore" => restore_items(conn, cfg, &ids(a)?),
        "reveal" => reveal(&text(a, "path")?, false),
        c if c.starts_with("history.") => crate::history::call(conn, c, a),
        c if c.starts_with("dups.settings.") || c.starts_with("folders.") || c.starts_with("settings.") || c == "exclusions.set" => {
            crate::settings::call(conn, cfg, c, a)
        }
        c if c.starts_with("dups.") => crate::dedupe::call(conn, cfg, c, a),
        c if c.starts_with("ai.") => crate::ai::call(conn, cfg, c, a),
        "openFile" => reveal(&text(a, "path")?, true),

        _ => Err(format!("unknown command: {cmd}")),
    }
}

pub(crate) fn add_to_album(conn: &mut Connection, album: i64, list: &[i64]) -> Result<Value, String> {
    let exists: bool = conn
        .query_row("SELECT 1 FROM albums WHERE id = ?1", [album], |_| Ok(true))
        .unwrap_or(false);
    if !exists {
        return Err("album not found".into());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut st = tx
            .prepare("INSERT OR IGNORE INTO album_items (album_id, media_id, added_at) VALUES (?1, ?2, ?3)")
            .map_err(|e| e.to_string())?;
        let t = now();
        let aname: String = tx.query_row("SELECT name FROM albums WHERE id = ?1", [album], |r| r.get(0)).unwrap_or_default();
        for m in list {
            if st.execute(params![album, m, t]).map_err(|e| e.to_string())? > 0 {
                crate::history::record(&tx, *m, "album_add", json!({ "albumId": album, "name": aname }));
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    crate::query::fts_refresh(conn, list);
    log(conn, "album.add", list.len(), &album.to_string());
    Ok(json!({ "updated": list.len() }))
}
