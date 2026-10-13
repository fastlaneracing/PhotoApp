//! Per-photo change history + undo.
//!
//! Every annotation change (tags, notes, AI description, rating, favorite,
//! color label, albums) is written to `change_log` with what it was before,
//! whether it came from the user or from an approved AI suggestion. Any single
//! change, a photo's whole history, or everything one AI run applied can be
//! reverted.
//!
//! Who made the change is carried in a thread-local (`with_source`) so the
//! library's edit functions don't need an extra parameter: the AI apply path
//! wraps its calls in `with_source(Source::ai(job, suggestion), ..)`.

use std::cell::Cell;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Source {
    pub ai: bool,
    pub job: Option<i64>,
    pub suggestion: Option<i64>,
}

impl Source {
    pub fn ai(job: i64, suggestion: i64) -> Self {
        Source { ai: true, job: Some(job), suggestion: Some(suggestion) }
    }
}

thread_local! {
    static CURRENT: Cell<Source> = Cell::new(Source::default());
}

/// Run `f` with changes attributed to `src` (restored afterwards).
pub fn with_source<T>(src: Source, f: impl FnOnce() -> T) -> T {
    let prev = CURRENT.with(|c| c.replace(src));
    let out = f();
    CURRENT.with(|c| c.set(prev));
    out
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Record one change for one item.
pub fn record(conn: &Connection, media_id: i64, action: &str, payload: Value) {
    let src = CURRENT.with(|c| c.get());
    let _ = conn.execute(
        "INSERT INTO change_log (at, source, job_id, suggestion_id, media_id, action, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            now(),
            if src.ai { "ai" } else { "user" },
            src.job,
            src.suggestion,
            media_id,
            action,
            payload.to_string()
        ],
    );
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn row_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let payload: String = r.get(7)?;
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "at": r.get::<_, i64>(1)?,
        "source": r.get::<_, String>(2)?,
        "jobId": r.get::<_, Option<i64>>(3)?,
        "suggestionId": r.get::<_, Option<i64>>(4)?,
        "mediaId": r.get::<_, i64>(5)?,
        "action": r.get::<_, String>(6)?,
        "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null),
        "undoneAt": r.get::<_, Option<i64>>(8)?,
    }))
}

const COLS: &str = "id, at, source, job_id, suggestion_id, media_id, action, payload, undone_at";

pub fn for_media(conn: &Connection, media_id: i64, limit: i64) -> Value {
    let rows: Vec<Value> = conn
        .prepare(&format!("SELECT {COLS} FROM change_log WHERE media_id = ?1 ORDER BY id DESC LIMIT ?2"))
        .and_then(|mut st| st.query_map(params![media_id, limit], row_json).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    Value::Array(rows)
}

pub fn recent(conn: &Connection, source: Option<&str>, limit: i64) -> Value {
    let rows: Vec<Value> = match source {
        Some(s) => conn
            .prepare(&format!("SELECT {COLS} FROM change_log WHERE source = ?1 ORDER BY id DESC LIMIT ?2"))
            .and_then(|mut st| st.query_map(params![s, limit], row_json).map(|it| it.flatten().collect())),
        None => conn
            .prepare(&format!("SELECT {COLS} FROM change_log ORDER BY id DESC LIMIT ?1"))
            .and_then(|mut st| st.query_map(params![limit], row_json).map(|it| it.flatten().collect())),
    }
    .unwrap_or_default();
    Value::Array(rows)
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

fn ann_get(conn: &Connection, media_id: i64, col: &str) -> Value {
    let sql = format!("SELECT {col} FROM annotations WHERE media_id = ?1");
    conn.query_row(&sql, [media_id], |r| {
        let v: rusqlite::types::Value = r.get(0)?;
        Ok(match v {
            rusqlite::types::Value::Null => Value::Null,
            rusqlite::types::Value::Integer(i) => json!(i),
            rusqlite::types::Value::Real(f) => json!(f),
            rusqlite::types::Value::Text(t) => json!(t),
            rusqlite::types::Value::Blob(_) => Value::Null,
        })
    })
    .unwrap_or(Value::Null)
}

fn to_sql(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(b) => rusqlite::types::Value::Integer(*b as i64),
        Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        other => rusqlite::types::Value::Text(other.to_string()),
    }
}

/// Field changes are only reverted when the field still holds what this change
/// set it to; a later edit wins and is reported instead of being clobbered.
fn same(a: &Value, b: &Value) -> bool {
    let norm = |v: &Value| match v {
        Value::Bool(x) => json!(*x as i64),
        Value::Null => json!(null),
        other => other.clone(),
    };
    let (a, b) = (norm(a), norm(b));
    a == b || (a.is_null() && b == json!("")) || (b.is_null() && a == json!(""))
}

/// Revert one change. Returns Ok(true) when reverted, Ok(false) when it was
/// already undone, Err(reason) on a conflict.
pub fn revert(conn: &Connection, change_id: i64, force: bool) -> Result<bool, String> {
    let row: Option<(i64, String, String, Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT media_id, action, payload, undone_at, suggestion_id FROM change_log WHERE id = ?1",
            [change_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((m, action, payload, undone, sugg)) = row else {
        return Err(format!("change {change_id} not found"));
    };
    if undone.is_some() {
        return Ok(false);
    }
    let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
    let _ = conn.execute("INSERT OR IGNORE INTO annotations (media_id, updated_at) VALUES (?1, ?2)", params![m, now()]);

    let field = |col: &str| -> Result<(), String> {
        let cur = ann_get(conn, m, col);
        if !force && !same(&cur, &p["after"]) {
            return Err(format!("{col} was changed again afterwards — undo the later change first"));
        }
        conn.execute(
            &format!("UPDATE annotations SET {col} = ?2, updated_at = ?3 WHERE media_id = ?1"),
            params![m, to_sql(&p["before"]), now()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    };

    match action.as_str() {
        "tag_add" => {
            conn.execute(
                "DELETE FROM media_tags WHERE media_id = ?1 AND tag_id = ?2",
                params![m, p["tagId"].as_i64()],
            )
            .map_err(|e| e.to_string())?;
        }
        "tag_remove" => {
            // The tag may have been deleted since: re-create it by name.
            let mut tid = p["tagId"].as_i64();
            let exists = tid
                .map(|t| conn.query_row("SELECT 1 FROM tags WHERE id = ?1", [t], |_| Ok(())).is_ok())
                .unwrap_or(false);
            if !exists {
                let name = p["name"].as_str().unwrap_or("").to_string();
                if name.is_empty() {
                    return Err("tag no longer exists".into());
                }
                let cat: Option<i64> = p["category"].as_str().and_then(|c| {
                    let _ = conn.execute("INSERT OR IGNORE INTO categories (name) VALUES (?1)", [c]);
                    conn.query_row("SELECT id FROM categories WHERE name = ?1", [c], |r| r.get(0)).ok()
                });
                let _ = conn.execute("INSERT OR IGNORE INTO tags (name, category_id) VALUES (?1, ?2)", params![name, cat]);
                tid = conn.query_row("SELECT id FROM tags WHERE name = ?1", [&name], |r| r.get(0)).ok();
            }
            conn.execute("INSERT OR IGNORE INTO media_tags (media_id, tag_id) VALUES (?1, ?2)", params![m, tid])
                .map_err(|e| e.to_string())?;
        }
        "notes" => field("notes")?,
        "description" => field("ai_description")?,
        "rating" => field("rating")?,
        "favorite" => field("favorite")?,
        "color" => field("color_label")?,
        "album_add" => {
            conn.execute(
                "DELETE FROM album_items WHERE album_id = ?1 AND media_id = ?2",
                params![p["albumId"].as_i64(), m],
            )
            .map_err(|e| e.to_string())?;
        }
        "album_remove" => {
            let ok = conn
                .query_row("SELECT 1 FROM albums WHERE id = ?1", [p["albumId"].as_i64()], |_| Ok(()))
                .is_ok();
            if !ok {
                return Err("album no longer exists".into());
            }
            conn.execute(
                "INSERT OR IGNORE INTO album_items (album_id, media_id, added_at) VALUES (?1, ?2, ?3)",
                params![p["albumId"].as_i64(), m, now()],
            )
            .map_err(|e| e.to_string())?;
        }
        other => return Err(format!("cannot undo '{other}'")),
    }
    conn.execute("UPDATE change_log SET undone_at = ?2 WHERE id = ?1", params![change_id, now()])
        .map_err(|e| e.to_string())?;
    if let Some(s) = sugg {
        // Undoing an AI change puts its suggestion back to "undone".
        let _ = conn.execute("UPDATE ai_suggestions SET status = 'undone' WHERE id = ?1", [s]);
    }
    crate::query::fts_refresh(conn, &[m]);
    Ok(true)
}

/// Revert a list of changes newest-first; conflicts are skipped and reported.
fn revert_many(conn: &Connection, ids: Vec<i64>) -> Value {
    let mut undone = 0;
    let mut conflicts: Vec<String> = vec![];
    for id in ids {
        match revert(conn, id, false) {
            Ok(true) => undone += 1,
            Ok(false) => {}
            Err(e) => conflicts.push(format!("change {id}: {e}")),
        }
    }
    json!({ "undone": undone, "conflicts": conflicts })
}

fn ids_where(conn: &Connection, sql: &str, p: i64) -> Vec<i64> {
    conn.prepare(sql)
        .and_then(|mut st| st.query_map([p], |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default()
}

pub fn undo_media(conn: &Connection, media_id: i64, ai_only: bool) -> Value {
    let sql = if ai_only {
        "SELECT id FROM change_log WHERE media_id = ?1 AND undone_at IS NULL AND source = 'ai' ORDER BY id DESC"
    } else {
        "SELECT id FROM change_log WHERE media_id = ?1 AND undone_at IS NULL ORDER BY id DESC"
    };
    revert_many(conn, ids_where(conn, sql, media_id))
}

pub fn undo_job(conn: &Connection, job_id: i64) -> Value {
    revert_many(
        conn,
        ids_where(conn, "SELECT id FROM change_log WHERE job_id = ?1 AND undone_at IS NULL ORDER BY id DESC", job_id),
    )
}

pub fn call(conn: &mut Connection, cmd: &str, a: &Value) -> Result<Value, String> {
    let int = |k: &str| a.get(k).and_then(|v| v.as_i64()).ok_or_else(|| format!("missing {k}"));
    match cmd {
        "history.media" => Ok(for_media(conn, int("id")?, a["limit"].as_i64().unwrap_or(200))),
        "history.recent" => Ok(recent(conn, a["source"].as_str(), a["limit"].as_i64().unwrap_or(300).clamp(1, 5000))),
        "history.undo" => {
            let force = a["force"].as_bool().unwrap_or(false);
            match revert(conn, int("changeId")?, force) {
                Ok(done) => Ok(json!({ "undone": done as i64, "conflicts": [] })),
                Err(e) => Err(e),
            }
        }
        "history.undoMedia" => Ok(undo_media(conn, int("id")?, a["aiOnly"].as_bool().unwrap_or(false))),
        "history.undoJob" => Ok(undo_job(conn, int("jobId")?)),
        _ => Err(format!("unknown command: {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(crate::db::SCHEMA).unwrap();
        c.execute_batch(
            "ALTER TABLE annotations ADD COLUMN ai_description TEXT NOT NULL DEFAULT '';
             INSERT INTO media (id, sha256, kind, ext, bytes) VALUES (1,'a','image','jpg',1);
             INSERT INTO annotations (media_id, notes) VALUES (1, 'old');
             CREATE VIRTUAL TABLE media_fts USING fts5(path, camera, ext, notes, tags, albums, description);",
        )
        .unwrap();
        c
    }

    #[test]
    fn revert_notes_and_conflict() {
        let c = db();
        c.execute("UPDATE annotations SET notes='new' WHERE media_id=1", []).unwrap();
        record(&c, 1, "notes", json!({"before": "old", "after": "new"}));
        c.execute("UPDATE annotations SET notes='newer' WHERE media_id=1", []).unwrap();
        record(&c, 1, "notes", json!({"before": "new", "after": "newer"}));
        // Undoing the older change first is a conflict.
        assert!(revert(&c, 1, false).is_err());
        let r = undo_media(&c, 1, false);
        assert_eq!(r["undone"], 2);
        let n: String = c.query_row("SELECT notes FROM annotations WHERE media_id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, "old");
    }

    #[test]
    fn ai_source_and_job_undo() {
        let c = db();
        c.execute("INSERT INTO tags (id, name) VALUES (7, 'Boat')", []).unwrap();
        c.execute("INSERT INTO media_tags VALUES (1, 7)", []).unwrap();
        with_source(Source::ai(5, 9), || record(&c, 1, "tag_add", json!({"tagId": 7, "name": "Boat"})));
        let src: String = c.query_row("SELECT source FROM change_log", [], |r| r.get(0)).unwrap();
        assert_eq!(src, "ai");
        assert_eq!(undo_job(&c, 5)["undone"], 1);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM media_tags", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // Thread-local restored.
        record(&c, 1, "rating", json!({"before": 0, "after": 3}));
        let src: String = c.query_row("SELECT source FROM change_log ORDER BY id DESC", [], |r| r.get(0)).unwrap();
        assert_eq!(src, "user");
    }
}
