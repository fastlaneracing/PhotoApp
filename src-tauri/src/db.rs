use rusqlite::Connection;

pub const SCHEMA: &str = "
-- One row per UNIQUE CONTENT. Identity is the content hash, not the path,
-- because the app performs moves itself and paths are not stable identities.
CREATE TABLE IF NOT EXISTS media (
  id INTEGER PRIMARY KEY,
  sha256 TEXT NOT NULL UNIQUE,
  kind TEXT NOT NULL CHECK (kind IN ('image','video')),
  ext TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  width INTEGER,
  height INTEGER,
  taken_at TEXT,
  date_source TEXT CHECK (date_source IN ('exif','sidecar','filename','mtime')),
  camera TEXT,
  gps_lat REAL,
  gps_lon REAL,
  thumb_path TEXT
);

-- One row per PATH where that content occurs. Dedupe flags live here:
-- a duplicate is a flag on an occurrence, never a removed file.
CREATE TABLE IF NOT EXISTS occurrence (
  id INTEGER PRIMARY KEY,
  media_id INTEGER NOT NULL REFERENCES media(id),
  path TEXT NOT NULL UNIQUE,
  root TEXT NOT NULL,
  mtime INTEGER NOT NULL,
  missing INTEGER NOT NULL DEFAULT 0,
  dup_group INTEGER,
  is_canonical INTEGER NOT NULL DEFAULT 0,
  -- M7: set when this occurrence was moved to the staging folder (flag, not
  -- deletion). Never cleared by the app; undo reverts the move, not the flag.
  is_flagged INTEGER NOT NULL DEFAULT 0,
  indexed_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_media_taken ON media(taken_at);
CREATE INDEX IF NOT EXISTS idx_media_kind ON media(kind);
CREATE INDEX IF NOT EXISTS idx_occ_root ON occurrence(root);
CREATE INDEX IF NOT EXISTS idx_occ_dup ON occurrence(dup_group);
CREATE INDEX IF NOT EXISTS idx_occ_media ON occurrence(media_id);

-- M7/M8: undo journal (charter: every applied op records its original path;
-- undo reverses in reverse order). Staging moves are journaled here.
CREATE TABLE IF NOT EXISTS op_journal (
  id INTEGER PRIMARY KEY,
  created_at INTEGER NOT NULL,
  op TEXT NOT NULL,
  media_id INTEGER NOT NULL,
  occ_id INTEGER NOT NULL,
  original_path TEXT NOT NULL,
  new_path TEXT,
  bytes INTEGER NOT NULL DEFAULT 0,
  cross_drive INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'applied' CHECK (status IN ('applied','undone','failed'))
);
CREATE INDEX IF NOT EXISTS idx_journal_media ON op_journal(media_id, status);

-- ---------------------------------------------------------------------------
-- Library management (organize + annotate). Everything below is keyed on
-- media_id = content identity, so notes/tags/ratings survive renames, moves
-- (in-app or in Explorer) and survive a file being edited in place (carried
-- over to the new content by scanner::upsert_entry).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS categories (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  color TEXT,
  sort INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS tags (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  category_id INTEGER REFERENCES categories(id)
);
CREATE TABLE IF NOT EXISTS media_tags (
  media_id INTEGER NOT NULL,
  tag_id INTEGER NOT NULL,
  PRIMARY KEY (media_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_media_tags_tag ON media_tags(tag_id);

CREATE TABLE IF NOT EXISTS annotations (
  media_id INTEGER PRIMARY KEY,
  notes TEXT NOT NULL DEFAULT '',
  rating INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
  favorite INTEGER NOT NULL DEFAULT 0,
  color_label TEXT,
  updated_at INTEGER
);

CREATE TABLE IF NOT EXISTS albums (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  description TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS album_items (
  album_id INTEGER NOT NULL,
  media_id INTEGER NOT NULL,
  added_at INTEGER NOT NULL,
  PRIMARY KEY (album_id, media_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_album_items_media ON album_items(media_id);

CREATE TABLE IF NOT EXISTS saved_searches (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  filter_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

-- Audit trail of every catalog edit made through the app.
CREATE TABLE IF NOT EXISTS activity_log (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  action TEXT NOT NULL,
  media_count INTEGER NOT NULL DEFAULT 0,
  detail TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS app_meta (key TEXT PRIMARY KEY, value TEXT);

-- A path whose content changed. Whether that was an edit (carry notes/tags to
-- the new content) or a rename shuffle (content just moved to another path) is
-- only knowable after every changed file has been seen, so the decision is
-- deferred to library::resolve_pending_carry.
-- ---------------------------------------------------------------------------
-- Change history: every annotation change, by the user or the AI, with what
-- it was before, so any single change, a whole photo's history or a whole AI
-- run can be undone.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS change_log (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  source TEXT NOT NULL,              -- 'user' | 'ai'
  job_id INTEGER,                    -- ai_jobs.id for AI changes
  suggestion_id INTEGER,             -- ai_suggestions.id for AI changes
  media_id INTEGER NOT NULL,
  action TEXT NOT NULL,              -- tag_add | tag_remove | notes | description | rating | favorite | color | album_add | album_remove
  payload TEXT NOT NULL,             -- JSON: what is needed to reverse it
  undone_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_change_media ON change_log(media_id, id);
CREATE INDEX IF NOT EXISTS idx_change_job ON change_log(job_id);

-- AI: a run over a set of photos (review) or an assistant request.
CREATE TABLE IF NOT EXISTS ai_jobs (
  id INTEGER PRIMARY KEY,
  created_at INTEGER NOT NULL,
  kind TEXT NOT NULL,                -- 'review' | 'assistant'
  label TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  total INTEGER NOT NULL DEFAULT 0,
  done INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'queued', -- queued | running | done | cancelled | error
  error TEXT,
  ids TEXT NOT NULL DEFAULT '[]',    -- JSON array of media ids
  finished_at INTEGER
);

-- AI proposals waiting for approval. Nothing touches the catalog until a
-- suggestion is approved; applying writes change_log rows that point back here.
CREATE TABLE IF NOT EXISTS ai_suggestions (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL,
  media_id INTEGER NOT NULL,
  kind TEXT NOT NULL,                -- add_tag | remove_tag | description | append_note | set_rating | add_to_album
  payload TEXT NOT NULL,             -- JSON
  status TEXT NOT NULL DEFAULT 'pending', -- pending | applied | rejected | undone | failed
  created_at INTEGER NOT NULL,
  decided_at INTEGER,
  error TEXT
);
CREATE INDEX IF NOT EXISTS idx_sugg_status ON ai_suggestions(status, media_id);
CREATE INDEX IF NOT EXISTS idx_sugg_job ON ai_suggestions(job_id);

-- Raw model output per photo per job (debugging / audit).
CREATE TABLE IF NOT EXISTS ai_responses (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL,
  media_id INTEGER NOT NULL,
  at INTEGER NOT NULL,
  ms INTEGER NOT NULL DEFAULT 0,
  ok INTEGER NOT NULL,
  text TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_airesp_media ON ai_responses(media_id);

CREATE TABLE IF NOT EXISTS pending_carry (
  old_media INTEGER NOT NULL,
  new_media INTEGER NOT NULL,
  at INTEGER NOT NULL,
  PRIMARY KEY (old_media, new_media)
) WITHOUT ROWID;
";

pub fn open(db_path: &str) -> Connection {
    let conn = Connection::open(db_path).expect("open catalog.db");
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    // The watcher thread, launch rescan and UI commands each open their own
    // connection; wait for a writer instead of failing with SQLITE_BUSY.
    conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
    // Schema setup + migrations run ONCE per process, serialized: on the first
    // launch after an upgrade the launch sync, the watcher and the first UI
    // calls all open the catalog at the same moment, and racing ALTER TABLEs
    // used to fail with "duplicate column". The IMMEDIATE transaction also
    // serializes against a second process (e.g. `photoapp.exe rescan`).
    static DONE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut done = DONE.lock().unwrap_or_else(|p| p.into_inner());
    if !done.iter().any(|d| d == db_path) {
        let _ = conn.execute_batch("BEGIN IMMEDIATE");
        conn.execute_batch(SCHEMA).expect("schema");
        migrate(&conn);
        let _ = conn.execute_batch("COMMIT");
        done.push(db_path.to_string());
    }
    conn
}

/// ALTER TABLE ADD COLUMN that tolerates the column already existing.
fn add_column(conn: &Connection, table: &str, col: &str, ddl: &str) {
    let has: bool = conn
        .query_row(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"), [col], |_| Ok(true))
        .unwrap_or(false);
    if !has {
        if let Err(e) = conn.execute(ddl, []) {
            if !e.to_string().contains("duplicate column") {
                panic!("migration {table}.{col}: {e}");
            }
        }
    }
}

/// Additive, idempotent migrations for catalogs created before a column
/// existed (SCHEMA's CREATE TABLE IF NOT EXISTS cannot evolve live tables).
fn migrate(conn: &Connection) {
    add_column(conn, "occurrence", "is_flagged", "ALTER TABLE occurrence ADD COLUMN is_flagged INTEGER NOT NULL DEFAULT 0");
    // Deleted-from-app marker: the item went to the Recycle Bin. Its notes and
    // tags are kept so a restore brings everything back.
    add_column(conn, "media", "deleted_at", "ALTER TABLE media ADD COLUMN deleted_at INTEGER");
    // AI-written description (kept apart from the user's own notes) and when
    // the item was last reviewed by the AI.
    add_column(conn, "annotations", "ai_description", "ALTER TABLE annotations ADD COLUMN ai_description TEXT NOT NULL DEFAULT ''");
    add_column(conn, "annotations", "ai_reviewed_at", "ALTER TABLE annotations ADD COLUMN ai_reviewed_at INTEGER");
    // Jobs left "running" by a crash/close can't still be running.
    let _ = conn.execute(
        "UPDATE ai_jobs SET status = 'cancelled', error = 'app closed while running', finished_at = strftime('%s','now')
         WHERE status IN ('queued','running')
           AND NOT EXISTS (SELECT 1 FROM app_meta WHERE key = 'ai_runner_pid' AND value = ?1)",
        [std::process::id().to_string()],
    );
    canonicalize_paths(conn);
    fix_filename_dates(conn);
}

/// One-time repair: dates taken from file names were stored as "2013-06-13_"
/// (stray underscore, time dropped). Re-derive them from the file name.
fn fix_filename_dates(conn: &Connection) {
    let done: bool = conn
        .query_row("SELECT 1 FROM app_meta WHERE key = 'fix_filename_dates_v1'", [], |_| Ok(true))
        .unwrap_or(false);
    if done {
        return;
    }
    let rows: Vec<(i64, String)> = conn
        .prepare(
            "SELECT m.id, (SELECT o.path FROM occurrence o WHERE o.media_id = m.id ORDER BY o.missing, o.id LIMIT 1)
             FROM media m WHERE m.date_source = 'filename' AND m.taken_at LIKE '%\\_%' ESCAPE '\\'",
        )
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    for (id, path) in rows {
        let name = path.rsplit(['\\', '/']).next().unwrap_or("");
        if let Some(d) = crate::scanner::date_from_filename(name) {
            let _ = conn.execute("UPDATE media SET taken_at = ?2 WHERE id = ?1", rusqlite::params![id, d]);
        }
    }
    let _ = conn.execute("INSERT OR REPLACE INTO app_meta (key, value) VALUES ('fix_filename_dates_v1', strftime('%s','now'))", []);
}

/// One-time: rewrite every occurrence.path to the single canonical form
/// (paths::canon). Where two rows collapse onto the same canonical path, keep
/// the present one and drop the other (annotations live on media, not here).
fn canonicalize_paths(conn: &Connection) {
    canonicalize_paths_with(conn, crate::paths::canon);
}

fn canonicalize_paths_with(conn: &Connection, canon: fn(&str) -> String) {
    let done: bool = conn
        .query_row("SELECT 1 FROM app_meta WHERE key = 'paths_canon_v1'", [], |_| Ok(true))
        .unwrap_or(false);
    if done {
        return;
    }
    let rows: Vec<(i64, String, i64)> = {
        let mut st = match conn.prepare("SELECT id, path, missing FROM occurrence") {
            Ok(s) => s,
            Err(_) => return,
        };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    };
    for (id, path, missing) in rows {
        let c = canon(&path);
        if c == path {
            continue;
        }
        let other: Option<(i64, i64)> = conn
            .query_row(
                "SELECT id, missing FROM occurrence WHERE path = ?1 AND id <> ?2",
                rusqlite::params![c, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match other {
            Some((oid, omissing)) => {
                // Keep whichever is present; tie -> keep the already-canonical row.
                if omissing == 1 && missing == 0 {
                    let _ = conn.execute("DELETE FROM occurrence WHERE id = ?1", [oid]);
                    let _ = conn.execute("UPDATE occurrence SET path = ?2 WHERE id = ?1", rusqlite::params![id, c]);
                } else {
                    let _ = conn.execute("DELETE FROM occurrence WHERE id = ?1", [id]);
                }
            }
            None => {
                let _ = conn.execute("UPDATE occurrence SET path = ?2 WHERE id = ?1", rusqlite::params![id, c]);
            }
        }
    }
    let _ = conn.execute(
        "INSERT OR REPLACE INTO app_meta(key, value) VALUES ('paths_canon_v1', strftime('%s','now'))",
        [],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(p: &str) -> String {
        p.replace('/', "\\")
    }

    #[test]
    fn canonicalize_merges_slash_variants() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute("INSERT INTO media (id, sha256, kind, ext, bytes) VALUES (1,'a','image','jpg',1),(2,'b','image','jpg',1)", []).unwrap();
        // 10: walk form, present. 11: same file stored by an old move, missing.
        // 12: a different file, walk form. 13: missing walk-form, canonical twin present (14).
        c.execute_batch(r"
            INSERT INTO occurrence (id, media_id, path, root, mtime, missing, indexed_at) VALUES
              (10, 1, 'E:/Photos\2010\x.jpg', 'E', 0, 0, 0),
              (11, 1, 'E:/Photos/2010/x.jpg',   'E', 0, 1, 0),
              (12, 2, 'E:/Photos\y.jpg',       'E', 0, 0, 0),
              (13, 2, 'E:/Photos\z.jpg',       'E', 0, 1, 0),
              (14, 2, 'E:\Photos\z.jpg',      'E', 0, 0, 0);
        ").unwrap();
        canonicalize_paths_with(&c, win);
        let rows: Vec<(i64, String, i64)> = c
            .prepare("SELECT id, path, missing FROM occurrence ORDER BY id").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
            .flatten().collect();
        assert_eq!(rows, vec![
            (10, r"E:\Photos\2010\x.jpg".to_string(), 0),
            (12, r"E:\Photos\y.jpg".to_string(), 0),
            (14, r"E:\Photos\z.jpg".to_string(), 0),
        ]);
        // Idempotent: second run is a no-op (flag set).
        canonicalize_paths_with(&c, win);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM occurrence", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }
}
