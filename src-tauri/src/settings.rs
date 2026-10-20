//! Settings: the library folders in config.toml (add, rename, pause, protect,
//! reorder, remove, exclusions) and the duplicate-handling options.
//!
//! Folders live in config.toml so they can still be edited by hand; the app
//! rewrites the file (keeping a `config.toml.bak` of the previous version) and
//! the next call picks the change up, because every command reloads the config.
//! Duplicate options live in the catalog (`app_meta.dup_settings`).

use std::path::Path;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::scanner::{Config, Root};

// ---------------------------------------------------------------------------
// Duplicate options
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DupSettings {
    /// Also find the same photo saved at a different size / quality
    /// (resized, re-compressed, converted), not only byte-identical copies.
    pub similar: bool,
    /// "strict" | "normal" | "loose": how alike two pictures must look.
    pub sensitivity: String,
    /// Similar photos must have the same date taken when both have one
    /// (camera EXIF or file name). Keeps burst shots and look-alikes apart.
    pub require_same_date: bool,
    /// "quality": keep the best copy wherever it is (folder order breaks ties).
    /// "folder": keep the copy in the highest-priority folder (quality breaks ties).
    pub keep_rule: String,
    /// Prefer names like IMG_1234.jpg over IMG_1234 (1).jpg / "Copy of …".
    pub prefer_original_names: bool,
    /// What happens to the copies that are not kept: "recycle" | "move".
    pub action: String,
    /// Folder for action = "move" (must be outside the library folders).
    pub move_dir: String,
    /// Give the kept copy the notes, tags, rating, favorite, label and albums
    /// of the copies that are removed.
    pub merge_annotations: bool,
    /// "all": duplicates anywhere. "acrossFolders": only when the copies are in
    /// different library folders.
    pub scope: String,
    /// Include videos (byte-identical copies only).
    pub include_videos: bool,
    /// After every sync: "off" | "exact" (identical copies) | "copies"
    /// (identical + clearly lower-quality copies). Look-alikes never auto-resolve.
    pub auto: String,
}

impl Default for DupSettings {
    fn default() -> Self {
        DupSettings {
            similar: true,
            sensitivity: "normal".into(),
            require_same_date: true,
            keep_rule: "quality".into(),
            prefer_original_names: true,
            action: "recycle".into(),
            move_dir: String::new(),
            merge_annotations: true,
            scope: "all".into(),
            include_videos: true,
            auto: "off".into(),
        }
    }
}

impl DupSettings {
    /// Max Hamming distance between two 64-bit picture fingerprints.
    pub fn max_distance(&self) -> u32 {
        match self.sensitivity.as_str() {
            "strict" => 3,
            "loose" => 10,
            _ => 6,
        }
    }
}

pub fn dup_settings(conn: &Connection) -> DupSettings {
    conn.query_row("SELECT value FROM app_meta WHERE key = 'dup_settings'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_dup_settings(conn: &Connection, cfg: &Config, a: &Value) -> Result<Value, String> {
    let mut cur = serde_json::to_value(dup_settings(conn)).map_err(|e| e.to_string())?;
    if let (Some(obj), Some(upd)) = (cur.as_object_mut(), a.as_object()) {
        for (k, v) in upd {
            if obj.contains_key(k) {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let mut s: DupSettings = serde_json::from_value(cur).map_err(|e| format!("bad setting: {e}"))?;
    let one_of = |v: &str, ok: &[&str], what: &str| -> Result<(), String> {
        if ok.contains(&v) { Ok(()) } else { Err(format!("{what} must be one of {}", ok.join(", "))) }
    };
    one_of(&s.sensitivity, &["strict", "normal", "loose"], "sensitivity")?;
    one_of(&s.keep_rule, &["quality", "folder"], "keepRule")?;
    one_of(&s.action, &["recycle", "move"], "action")?;
    one_of(&s.scope, &["all", "acrossFolders"], "scope")?;
    one_of(&s.auto, &["off", "exact", "copies"], "auto")?;
    s.move_dir = s.move_dir.trim().trim_end_matches(['\\', '/']).to_string();
    if s.action == "move" {
        if s.move_dir.is_empty() {
            return Err("choose a folder for the removed copies".into());
        }
        if let Some(r) = overlapping_root(cfg, &s.move_dir) {
            return Err(format!(
                "the folder for removed copies must be outside your library folders (it is inside or contains \"{}\")",
                r.name
            ));
        }
        std::fs::create_dir_all(&s.move_dir).map_err(|e| format!("cannot use {}: {e}", s.move_dir))?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('dup_settings', ?1)",
        [serde_json::to_string(&s).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(&s).unwrap_or(json!({})))
}

// ---------------------------------------------------------------------------
// config.toml
// ---------------------------------------------------------------------------

fn toml_str(s: &str) -> String {
    // Basic string with escapes (\\ and \").
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Paths are written with forward slashes, the form the file's comments suggest.
fn file_path(p: &str) -> String {
    let p = p.trim_start_matches(r"\\?\").replace('\\', "/");
    if p.len() > 3 { p.trim_end_matches('/').to_string() } else { p }
}

pub fn config_text(all: &[Root], exclusions: &[String], ffmpeg: Option<&str>) -> String {
    let mut s = String::from(
        "# photoApp configuration. Managed by the app (Settings tab); hand edits are fine too.\n\
         # Restart the app after editing by hand. The previous version is kept as config.toml.bak.\n\
         # Use forward slashes in paths, e.g. \"D:/Photos\".\n\
         # NOTE: top-level keys must come BEFORE the [[roots]] tables.\n\n\
         # Folders (path prefixes, case-insensitive) to skip while scanning.\n",
    );
    s.push_str("exclusions = [\n");
    for e in exclusions {
        s.push_str(&format!("  {},\n", toml_str(&file_path(e))));
    }
    s.push_str("]\n\n");
    match ffmpeg {
        Some(f) if !f.trim().is_empty() => s.push_str(&format!("ffmpeg = {}\n\n", toml_str(f))),
        _ => s.push_str("# Optional: full path to ffmpeg.exe for video thumbnails. Default: ffmpeg on PATH.\n# ffmpeg = \"C:/tools/ffmpeg/bin/ffmpeg.exe\"\n\n"),
    }
    s.push_str(
        "# Library folders, highest priority first. The first one is also the default\n\
         # destination for \"move to year folder\".\n\
         #   enabled = false   paused: not scanned or watched, catalog left as is\n\
         #   protected = true  photoApp never deletes or moves files in this folder\n",
    );
    for r in all {
        s.push_str(&format!(
            "\n[[roots]]\nname = {}\npath = {}\nenabled = {}\nprotected = {}\n",
            toml_str(&r.name),
            toml_str(&file_path(&r.path)),
            r.enabled,
            r.protected
        ));
    }
    s
}

fn write_config(all: &[Root], cfg: &Config) -> Result<(), String> {
    write_config_with(all, &cfg.exclusions, cfg.ffmpeg.as_deref())
}

fn write_config_with(all: &[Root], exclusions: &[String], ffmpeg: Option<&str>) -> Result<(), String> {
    let text = config_text(all, exclusions, ffmpeg);
    // Never write a file the loader would reject.
    let parsed: Config = toml::from_str(&text).map_err(|e| format!("internal: config would not parse: {e}"))?;
    if parsed.roots.len() != all.len() {
        return Err("internal: config round-trip lost folders".into());
    }
    let path = crate::paths::config_path();
    let p = Path::new(&path);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if p.exists() {
        let _ = std::fs::copy(p, p.with_extension("toml.bak"));
    }
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("cannot replace {}: {e}", p.display()))?;
    Ok(())
}

fn key_dir(p: &str) -> String {
    crate::paths::key(p).trim_end_matches('\\').to_string()
}

fn inside(child: &str, parent: &str) -> bool {
    let (c, p) = (key_dir(child), key_dir(parent));
    c == p || c.starts_with(&format!("{p}\\"))
}

/// A configured folder that is the same as, inside, or contains `dir`.
fn overlapping_root<'a>(cfg: &'a Config, dir: &str) -> Option<&'a Root> {
    cfg.roots.iter().chain(cfg.paused.iter()).find(|r| inside(dir, &r.path) || inside(&r.path, dir))
}

fn find(all: &[Root], name: &str) -> Result<usize, String> {
    all.iter().position(|r| r.name == name).ok_or_else(|| format!("no library folder named \"{name}\""))
}

fn clean_name(s: &str) -> Result<String, String> {
    let n = s.trim().to_string();
    if n.is_empty() {
        return Err("folder name cannot be empty".into());
    }
    if n.len() > 80 {
        return Err("folder name is too long".into());
    }
    Ok(n)
}

fn unique_name(all: &[Root], base: &str) -> String {
    let base = if base.trim().is_empty() { "Photos".to_string() } else { base.trim().to_string() };
    if !all.iter().any(|r| r.name.eq_ignore_ascii_case(&base)) {
        return base;
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !all.iter().any(|r| r.name.eq_ignore_ascii_case(n))).unwrap()
}

fn add_folder(conn: &Connection, cfg: &Config, a: &Value) -> Result<Value, String> {
    let raw = a.get("path").and_then(|v| v.as_str()).unwrap_or("").trim();
    if raw.is_empty() {
        return Err("choose a folder".into());
    }
    let path = crate::paths::canon(raw);
    let path = if path.len() > 3 { path.trim_end_matches(['\\', '/']).to_string() } else { path };
    if !Path::new(&path).is_dir() {
        return Err(format!("folder not found: {path}"));
    }
    if let Some(r) = overlapping_root(cfg, &path) {
        return Err(if key_dir(&r.path) == key_dir(&path) {
            format!("{path} is already in the library as \"{}\"", r.name)
        } else if inside(&path, &r.path) {
            format!("{path} is inside \"{}\" ({}), which is already scanned", r.name, r.path)
        } else {
            format!("{path} contains the library folder \"{}\" ({}); remove that one first or add a different folder", r.name, r.path)
        });
    }
    let data = crate::paths::data_dir().to_string_lossy().to_string();
    if inside(&data, &path) || inside(&path, &data) {
        return Err("that folder holds photoApp's own data (catalog, thumbnails)".into());
    }
    let ds = dup_settings(conn);
    if ds.action == "move" && !ds.move_dir.is_empty() && (inside(&path, &ds.move_dir) || inside(&ds.move_dir, &path)) {
        return Err("that folder overlaps the folder where removed duplicates go (Settings → Duplicates)".into());
    }
    let mut all = cfg.all_roots();
    let base = a
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| path.trim_end_matches(['\\', '/', ':']).to_string())
        });
    let name = unique_name(&all, &clean_name(&base)?);
    let protected = a.get("protected").and_then(|v| v.as_bool()).unwrap_or(false);
    all.push(Root { name: name.clone(), path: path.clone(), enabled: true, protected });
    write_config(&all, cfg)?;
    crate::library::log_activity(conn, "folder.add", 0, &format!("{name} = {path}"));
    Ok(json!({ "name": name, "path": path, "folderChanged": true, "syncNeeded": true }))
}

fn update_folder(conn: &Connection, cfg: &Config, a: &Value) -> Result<Value, String> {
    let name = a.get("name").and_then(|v| v.as_str()).ok_or("missing name")?;
    let mut all = cfg.all_roots();
    let i = find(&all, name)?;
    let mut changes = vec![];
    let mut sync_needed = false;
    if let Some(n) = a.get("newName").and_then(|v| v.as_str()) {
        let n = clean_name(n)?;
        if n != all[i].name {
            if all.iter().enumerate().any(|(j, r)| j != i && r.name.eq_ignore_ascii_case(&n)) {
                return Err(format!("another folder is already called \"{n}\""));
            }
            conn.execute("UPDATE occurrence SET root = ?2 WHERE root = ?1", params![all[i].name, n])
                .map_err(|e| e.to_string())?;
            changes.push(format!("renamed {} → {n}", all[i].name));
            all[i].name = n;
        }
    }
    if let Some(e) = a.get("enabled").and_then(|v| v.as_bool()) {
        if e != all[i].enabled {
            all[i].enabled = e;
            changes.push(if e { "resumed".into() } else { "paused".to_string() });
            sync_needed |= e;
        }
    }
    if let Some(p) = a.get("protected").and_then(|v| v.as_bool()) {
        if p != all[i].protected {
            all[i].protected = p;
            changes.push(if p { "protected".into() } else { "unprotected".to_string() });
        }
    }
    if let Some(d) = a.get("move").and_then(|v| v.as_i64()) {
        let j = (i as i64 + d).clamp(0, all.len() as i64 - 1) as usize;
        if j != i {
            let r = all.remove(i);
            all.insert(j, r);
            changes.push(format!("priority {} → {}", i + 1, j + 1));
        }
    }
    if changes.is_empty() {
        return Ok(json!({ "ok": true, "folderChanged": false }));
    }
    write_config(&all, cfg)?;
    crate::library::log_activity(conn, "folder.update", 0, &format!("{name}: {}", changes.join(", ")));
    Ok(json!({ "ok": true, "folderChanged": true, "syncNeeded": sync_needed }))
}

/// What removing a folder would do: items whose only copies are there drop
/// out of the library (notes kept, listed under "Not on disk").
fn remove_preview(conn: &Connection, name: &str) -> Value {
    let q = |sql: &str| -> i64 { conn.query_row(sql, [name], |r| r.get(0)).unwrap_or(0) };
    let only_here = "SELECT DISTINCT o.media_id FROM occurrence o WHERE o.root = ?1 AND o.missing = 0
                     AND NOT EXISTS (SELECT 1 FROM occurrence x WHERE x.media_id = o.media_id AND x.missing = 0 AND x.root <> ?1)";
    json!({
        "items": q("SELECT COUNT(DISTINCT media_id) FROM occurrence WHERE root = ?1 AND missing = 0"),
        "onlyHere": q(&format!("SELECT COUNT(*) FROM ({only_here})")),
        "onlyHereAnnotated": q(&format!(
            "SELECT COUNT(*) FROM ({only_here}) t WHERE EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id = t.media_id)
               OR EXISTS (SELECT 1 FROM annotations a WHERE a.media_id = t.media_id AND (a.notes <> '' OR a.rating > 0 OR a.favorite = 1 OR a.color_label IS NOT NULL))
               OR EXISTS (SELECT 1 FROM album_items al WHERE al.media_id = t.media_id)")),
    })
}

fn remove_folder(conn: &mut Connection, cfg: &Config, a: &Value) -> Result<Value, String> {
    let name = a.get("name").and_then(|v| v.as_str()).ok_or("missing name")?;
    let mut all = cfg.all_roots();
    let i = find(&all, name)?;
    if a.get("preview").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(remove_preview(conn, name));
    }
    if all.len() == 1 {
        return Err("the library needs at least one folder — add the new one first, then remove this one".into());
    }
    let r = all.remove(i);
    write_config(&all, cfg)?;
    // Its files are no longer part of the library. Rows are kept (notes, tags
    // and history survive); adding the folder again brings them straight back.
    let n = conn
        .execute(
            "UPDATE occurrence SET missing = 1, is_canonical = 0, dup_group = NULL, indexed_at = strftime('%s','now')
             WHERE root = ?1 AND missing = 0",
            [&r.name],
        )
        .map_err(|e| e.to_string())?;
    let new_cfg = crate::scanner::load_config(Path::new(&crate::paths::config_path()));
    crate::library::recompute_dups(conn, &new_cfg);
    crate::query::force_rebuild_fts(conn);
    crate::library::log_activity(conn, "folder.remove", n, &format!("{} ({}) removed from the library; files on disk untouched", r.name, r.path));
    Ok(json!({ "ok": true, "paths": n, "folderChanged": true }))
}

fn set_exclusions(conn: &Connection, cfg: &Config, a: &Value) -> Result<Value, String> {
    let list: Vec<String> = a
        .get("list")
        .and_then(|v| v.as_array())
        .ok_or("missing list")?
        .iter()
        .filter_map(|x| x.as_str())
        .map(|s| s.trim().trim_end_matches(['\\', '/']).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    write_config_with(&cfg.all_roots(), &list, cfg.ffmpeg.as_deref())?;
    crate::library::log_activity(conn, "exclusions.set", list.len(), &list.join("; "));
    Ok(json!({ "ok": true, "folderChanged": true, "syncNeeded": true }))
}

pub fn summary(conn: &Connection, cfg: &Config) -> Value {
    let counts = |sql: &str| -> std::collections::HashMap<String, i64> {
        conn.prepare(sql)
            .and_then(|mut st| st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map(|it| it.flatten().collect()))
            .unwrap_or_default()
    };
    let items = counts("SELECT root, COUNT(DISTINCT media_id) FROM occurrence WHERE missing = 0 GROUP BY root");
    let bytes = counts(
        "SELECT o.root, SUM(m.bytes) FROM occurrence o JOIN media m ON m.id = o.media_id WHERE o.missing = 0 GROUP BY o.root",
    );
    let folders: Vec<Value> = cfg
        .all_roots()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({
                "name": r.name, "path": r.path, "enabled": r.enabled, "protected": r.protected,
                "priority": i + 1, "online": Path::new(&r.path).is_dir(),
                "items": items.get(&r.name).copied().unwrap_or(0),
                "bytes": bytes.get(&r.name).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({
        "folders": folders,
        "exclusions": cfg.exclusions,
        "configPath": crate::paths::config_path(),
        "dups": dup_settings(conn),
    })
}

pub fn call(conn: &mut Connection, cfg: &Config, cmd: &str, a: &Value) -> Result<Value, String> {
    match cmd {
        "settings.get" => Ok(summary(conn, cfg)),
        "folders.add" => add_folder(conn, cfg, a),
        "folders.update" => update_folder(conn, cfg, a),
        "folders.remove" => remove_folder(conn, cfg, a),
        "exclusions.set" => set_exclusions(conn, cfg, a),
        "dups.settings.set" => save_dup_settings(conn, cfg, a),
        _ => Err(format!("unknown command: {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_text_round_trips() {
        let all = vec![
            Root { name: "E-Photos".into(), path: r"E:\Photos".into(), enabled: true, protected: false },
            Root { name: "Backup \"old\"".into(), path: "F:/Backup".into(), enabled: false, protected: true },
        ];
        let t = config_text(&all, &["D:/Skip".into()], Some("C:/ff/ffmpeg.exe"));
        let c: Config = toml::from_str(&t).unwrap();
        assert_eq!(c.roots.len(), 2);
        assert_eq!(c.roots[0].path, "E:/Photos");
        assert_eq!(c.roots[1].name, "Backup \"old\"");
        assert!(!c.roots[1].enabled && c.roots[1].protected);
        assert_eq!(c.exclusions, vec!["D:/Skip".to_string()]);
        assert_eq!(c.ffmpeg.as_deref(), Some("C:/ff/ffmpeg.exe"));
    }

    #[test]
    fn nesting_is_detected() {
        assert!(inside(r"E:\Photos\2019", "E:/Photos"));
        assert!(inside("E:/Photos", r"e:\photos\"));
        assert!(!inside(r"E:\Photos2", "E:/Photos"));
    }

    #[test]
    fn dup_settings_default_when_missing() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE app_meta (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        assert_eq!(dup_settings(&c), DupSettings::default());
        c.execute("INSERT INTO app_meta VALUES ('dup_settings', '{\"sensitivity\":\"loose\"}')", []).unwrap();
        let s = dup_settings(&c);
        assert_eq!(s.max_distance(), 10);
        assert!(s.similar); // missing keys take defaults
    }
}
