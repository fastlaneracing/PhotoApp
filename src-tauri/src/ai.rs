//! AI assistant: connect an OpenAI-compatible (LM Studio, Ollama, llama.cpp,
//! vLLM, OpenAI) or Anthropic-compatible endpoint and let a (vision) model
//! review, describe, tag and find photos.
//!
//! Hard rule: the model never changes the catalog directly. Everything it
//! proposes lands in `ai_suggestions` as *pending*; the user approves or
//! rejects. Approved changes go through the normal library functions inside
//! `history::with_source(Source::ai(..))`, so every one is recorded per photo
//! in `change_log` and can be undone individually, per photo, or per job.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::{self, Source};
use crate::scanner::Config;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// "openai" (any OpenAI-compatible server) | "anthropic"
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Send the picture to the model (needs a vision model). Off = text-only
    /// review from file name / folder / date.
    pub vision: bool,
    /// Ask the server for strict JSON (OpenAI `response_format`). Some local
    /// servers reject it, so it is off by default; replies are parsed leniently.
    pub json_mode: bool,
    pub temperature: f64,
    pub max_tokens: u32,
    pub timeout_secs: u64,
    /// Longest side of the image sent to the model.
    pub image_max_px: u32,
    pub max_tags: u32,
    /// Preferred tag categories, comma separated.
    pub categories: String,
    /// Extra instructions, e.g. "Boats are APBA offshore race boats; read hull numbers."
    pub instructions: String,
    /// Let the model propose a note (visible text, numbers, location hints).
    pub suggest_notes: bool,
    /// Settings format version (for one-time default upgrades).
    pub settings_version: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            provider: "openai".into(),
            base_url: "http://localhost:1234/v1".into(),
            api_key: String::new(),
            model: String::new(),
            vision: true,
            json_mode: false,
            temperature: 0.2,
            max_tokens: 4096, // room for "thinking" models
            timeout_secs: 300,
            image_max_px: 1024,
            max_tags: 8,
            categories: "Subject, People, Place, Vehicle, Event, Activity, Object".into(),
            instructions: String::new(),
            suggest_notes: true,
            settings_version: 1,
        }
    }
}

pub fn load_settings(conn: &Connection) -> Settings {
    let raw: Option<Value> = conn
        .query_row("SELECT value FROM app_meta WHERE key = 'ai_settings'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let Some(raw) = raw else { return Settings::default() };
    let mut s: Settings = serde_json::from_value(raw.clone()).unwrap_or_default();
    // v0.4.0 saved settings had no version and a 1024-token default, too small
    // for "thinking" vision models. Upgrade that untouched default once.
    if raw.get("settingsVersion").is_none() {
        if s.max_tokens == 1024 {
            s.max_tokens = 4096;
        }
        s.settings_version = 1;
    }
    s
}

fn public_settings(s: &Settings) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(json!({}));
    v["hasKey"] = json!(!s.api_key.is_empty());
    v["apiKey"] = json!(""); // never sent back to the UI
    v
}

fn save_settings(conn: &Connection, patch: &Value) -> Result<Settings, String> {
    let mut cur = serde_json::to_value(load_settings(conn)).map_err(|e| e.to_string())?;
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            // An empty apiKey field means "keep the stored key"; clearKey removes it.
            if k == "apiKey" && v.as_str().map(|s| s.is_empty()).unwrap_or(true) {
                continue;
            }
            if k == "clearKey" {
                continue;
            }
            cur[k] = v.clone();
        }
        if patch["clearKey"].as_bool() == Some(true) {
            cur["apiKey"] = json!("");
        }
    }
    let s: Settings = serde_json::from_value(cur).map_err(|e| format!("bad settings: {e}"))?;
    if !matches!(s.provider.as_str(), "openai" | "anthropic") {
        return Err("provider must be openai or anthropic".into());
    }
    if !(s.base_url.starts_with("http://") || s.base_url.starts_with("https://")) {
        return Err("base URL must start with http:// or https://".into());
    }
    conn.execute(
        "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('ai_settings', ?1)",
        [serde_json::to_string(&s).unwrap_or_default()],
    )
    .map_err(|e| e.to_string())?;
    Ok(s)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn agent(s: &Settings) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(s.timeout_secs.clamp(10, 3600)))
        .build()
}

fn base(s: &Settings) -> String {
    s.base_url.trim().trim_end_matches('/').to_string()
}

/// Anthropic paths live under /v1; accept a base URL with or without it.
fn anthropic_url(s: &Settings, path: &str) -> String {
    let b = base(s);
    if b.ends_with("/v1") { format!("{b}/{path}") } else { format!("{b}/v1/{path}") }
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"].as_str().map(|s| s.to_string())
                        .or_else(|| v["error"].as_str().map(|s| s.to_string()))
                        .or_else(|| v["message"].as_str().map(|s| s.to_string()))
                })
                .unwrap_or_else(|| body.chars().take(300).collect());
            format!("HTTP {code}: {msg}")
        }
        ureq::Error::Transport(t) => format!("cannot reach the AI server: {t}"),
    }
}

fn request(s: &Settings, method: &str, url: &str) -> ureq::Request {
    let mut r = agent(s).request(method, url).set("Content-Type", "application/json");
    if s.provider == "anthropic" {
        r = r.set("anthropic-version", "2023-06-01");
        if !s.api_key.is_empty() {
            r = r.set("x-api-key", &s.api_key);
        }
    } else if !s.api_key.is_empty() {
        r = r.set("Authorization", &format!("Bearer {}", s.api_key));
    }
    r
}

pub struct Img {
    pub mime: &'static str,
    pub b64: String,
}

/// One chat turn. Returns the model's text.
pub fn chat(s: &Settings, system: &str, text: &str, image: Option<&Img>) -> Result<String, String> {
    if s.model.trim().is_empty() {
        return Err("no model selected — set one in AI settings".into());
    }
    if s.provider == "anthropic" {
        let mut content = vec![];
        if let Some(i) = image {
            content.push(json!({ "type": "image", "source": { "type": "base64", "media_type": i.mime, "data": i.b64 } }));
        }
        content.push(json!({ "type": "text", "text": text }));
        let body = json!({
            "model": s.model, "max_tokens": s.max_tokens, "temperature": s.temperature,
            "system": system, "messages": [{ "role": "user", "content": content }],
        });
        let v: Value = request(s, "POST", &anthropic_url(s, "messages"))
            .send_json(body)
            .map_err(http_err)?
            .into_json()
            .map_err(|e| format!("bad response: {e}"))?;
        let out: String = v["content"]
            .as_array()
            .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""))
            .unwrap_or_default();
        if out.is_empty() {
            if v["stop_reason"].as_str() == Some("max_tokens") {
                return Err(format!(
                    "the model used all {} reply tokens without answering — raise \"Max reply tokens\" in AI settings",
                    s.max_tokens
                ));
            }
            return Err(format!("empty reply: {}", v.to_string().chars().take(300).collect::<String>()));
        }
        Ok(out)
    } else {
        let user_content = match image {
            Some(i) => json!([
                { "type": "text", "text": text },
                { "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", i.mime, i.b64) } }
            ]),
            None => json!(text),
        };
        let mut body = json!({
            "model": s.model, "temperature": s.temperature, "max_tokens": s.max_tokens, "stream": false,
            "messages": [ { "role": "system", "content": system }, { "role": "user", "content": user_content } ],
        });
        if s.json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }
        let v: Value = request(s, "POST", &format!("{}/chat/completions", base(s)))
            .send_json(body)
            .map_err(http_err)?
            .into_json()
            .map_err(|e| format!("bad response: {e}"))?;
        let msg = &v["choices"][0]["message"];
        let out = match &msg["content"] {
            Value::String(t) => t.clone(),
            Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
            _ => String::new(),
        };
        if out.trim().is_empty() {
            // "Thinking" models (Qwen3, DeepSeek-R1, …) put their reasoning in
            // reasoning_content and can run out of tokens before answering.
            let reasoning = msg["reasoning_content"].as_str().or_else(|| msg["reasoning"].as_str()).unwrap_or("");
            if let Some(j) = extract_json(reasoning) {
                return Ok(j.to_string()); // the answer was already in the reasoning
            }
            if v["choices"][0]["finish_reason"].as_str() == Some("length") && !reasoning.is_empty() {
                return Err(format!(
                    "the model spent all {} reply tokens thinking and never answered — raise \"Max reply tokens\" in AI settings (try 4096 or more) or use a non-thinking model",
                    s.max_tokens
                ));
            }
            return Err(format!("empty reply: {}", v.to_string().chars().take(300).collect::<String>()));
        }
        Ok(out)
    }
}

pub fn list_models(s: &Settings) -> Result<Vec<String>, String> {
    let url = if s.provider == "anthropic" { anthropic_url(s, "models") } else { format!("{}/models", base(s)) };
    let v: Value = request(s, "GET", &url)
        .call()
        .map_err(http_err)?
        .into_json()
        .map_err(|e| format!("bad response: {e}"))?;
    let mut out: Vec<String> = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .map(|a| a.iter().filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()).map(String::from)).collect())
        .unwrap_or_default();
    out.sort();
    Ok(out)
}

// ---------------------------------------------------------------------------
// Lenient JSON extraction (local models wrap JSON in prose, fences, <think>)
// ---------------------------------------------------------------------------

pub fn extract_json(text: &str) -> Option<Value> {
    let mut t = text.to_string();
    while let Some(a) = t.find("<think>") {
        match t[a..].find("</think>") {
            Some(b) => t.replace_range(a..a + b + "</think>".len(), ""),
            None => {
                t.truncate(a);
                break;
            }
        }
    }
    if let Ok(v) = serde_json::from_str::<Value>(t.trim()) {
        if v.is_object() {
            return Some(v);
        }
    }
    let bytes = t.as_bytes();
    for start in 0..bytes.len() {
        if bytes[start] != b'{' {
            continue;
        }
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for i in start..bytes.len() {
            let c = bytes[i];
            if in_str {
                if esc {
                    esc = false;
                } else if c == b'\\' {
                    esc = true;
                } else if c == b'"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v) = serde_json::from_str::<Value>(&t[start..=i]) {
                            if v.is_object() {
                                return Some(v);
                            }
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Image preparation
// ---------------------------------------------------------------------------

fn encode_jpeg(img: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let rgb = img.to_rgb8();
    let mut buf: Vec<u8> = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

fn decode_oriented(path: &str) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut dec = reader.into_decoder().map_err(|e| e.to_string())?;
    let orient = dec.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orient);
    Ok(img)
}

/// The picture the model sees: the original photo (EXIF rotation applied,
/// downsized to `image_max_px`), or the thumbnail for videos / formats the
/// decoder can't read (HEIC).
pub fn prepare_image(conn: &Connection, media_id: i64, max_px: u32) -> Result<Img, String> {
    let (kind, thumb): (String, Option<String>) = conn
        .query_row("SELECT kind, thumb_path FROM media WHERE id = ?1", [media_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|_| "item not found".to_string())?;
    let path: Option<String> = conn
        .query_row(
            "SELECT path FROM occurrence WHERE media_id = ?1 AND missing = 0 ORDER BY is_canonical DESC, id LIMIT 1",
            [media_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let max = max_px.clamp(256, 4096);
    if kind == "image" {
        if let Some(p) = &path {
            if let Ok(img) = decode_oriented(p) {
                let img = if img.width() > max || img.height() > max {
                    img.resize(max, max, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                let bytes = encode_jpeg(&img)?;
                return Ok(Img { mime: "image/jpeg", b64: base64::engine::general_purpose::STANDARD.encode(bytes) });
            }
        }
    }
    match thumb.filter(|t| std::path::Path::new(t).exists()) {
        Some(t) => {
            let bytes = std::fs::read(&t).map_err(|e| e.to_string())?;
            Ok(Img { mime: "image/jpeg", b64: base64::engine::general_purpose::STANDARD.encode(bytes) })
        }
        None => Err(if path.is_none() { "file is not on disk".into() } else { "no preview available for this file".into() }),
    }
}

// ---------------------------------------------------------------------------
// Review: prompt + parsing
// ---------------------------------------------------------------------------

struct ItemCtx {
    name: String,
    folder: String,
    kind: String,
    taken_at: String,
    date_source: String,
    camera: String,
    tags: Vec<String>,
    notes: String,
    description: String,
}

fn item_ctx(conn: &Connection, media_id: i64) -> Result<ItemCtx, String> {
    let (kind, taken, ds, cam): (String, Option<String>, Option<String>, Option<String>) = conn
        .query_row("SELECT kind, taken_at, date_source, camera FROM media WHERE id = ?1", [media_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|_| "item not found".to_string())?;
    let path: String = conn
        .query_row(
            "SELECT path FROM occurrence WHERE media_id = ?1 ORDER BY missing, is_canonical DESC, id LIMIT 1",
            [media_id],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let i = path.rfind(['\\', '/']).map(|i| i + 1).unwrap_or(0);
    let tags: Vec<String> = conn
        .prepare(
            "SELECT COALESCE(c.name || ': ', '') || t.name FROM media_tags mt JOIN tags t ON t.id = mt.tag_id
             LEFT JOIN categories c ON c.id = t.category_id WHERE mt.media_id = ?1",
        )
        .and_then(|mut st| st.query_map([media_id], |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    let (notes, description): (String, String) = conn
        .query_row("SELECT notes, ai_description FROM annotations WHERE media_id = ?1", [media_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap_or_default();
    Ok(ItemCtx {
        name: path[i..].to_string(),
        folder: path[..i].trim_end_matches(['\\', '/']).to_string(),
        kind,
        taken_at: taken.unwrap_or_default(),
        date_source: ds.unwrap_or_default(),
        camera: cam.unwrap_or_default(),
        tags,
        notes,
        description,
    })
}

/// "Category: name (count)" for the most-used tags, so the model reuses them.
fn vocabulary(conn: &Connection, limit: i64) -> String {
    let rows: Vec<String> = conn
        .prepare(
            "SELECT COALESCE(c.name || ': ', '') || t.name, COUNT(mt.media_id) AS n
             FROM tags t LEFT JOIN categories c ON c.id = t.category_id
             LEFT JOIN media_tags mt ON mt.tag_id = t.id
             GROUP BY t.id ORDER BY n DESC, t.name LIMIT ?1",
        )
        .and_then(|mut st| {
            st.query_map([limit], |r| Ok(format!("{} ({})", r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    if rows.is_empty() { "(none yet)".into() } else { rows.join("; ") }
}

fn review_system_prompt(s: &Settings, vocab: &str) -> String {
    format!(
        "You are a meticulous photo librarian cataloging a private photo and video library.\n\
Return ONLY one JSON object - no prose, no markdown, no code fences:\n\
{{\"description\": \"...\", \"tags\": [{{\"category\": \"...\", \"name\": \"...\"}}], \"note\": \"...\"}}\n\
Rules:\n\
- description: one or two plain, factual sentences about what is visible: main subjects, setting, notable objects, \
any readable text or signage, activity, time of day / weather. Do not guess who people are.\n\
- tags: at most {max} short tags (1-3 words, Title Case), each with a category. Prefer these categories: {cats}. \
When an existing tag below fits, REUSE its exact name and category instead of inventing a near-duplicate. \
Do not repeat tags the item already has. Only tag what is clearly visible.\n\
- note: {note_rule}\n\
{extra}\
Existing tags (category: name (uses)): {vocab}",
        max = s.max_tags.clamp(1, 30),
        cats = if s.categories.trim().is_empty() { "any sensible category".to_string() } else { s.categories.trim().to_string() },
        note_rule = if s.suggest_notes {
            "only facts worth remembering that are not in the description (visible text, registration / hull / plate numbers, a likely place); otherwise \"\"."
        } else {
            "always \"\"."
        },
        extra = if s.instructions.trim().is_empty() { String::new() } else { format!("Owner's instructions: {}\n", s.instructions.trim()) },
        vocab = vocab,
    )
}

fn review_user_text(c: &ItemCtx, vision: bool) -> String {
    let mut t = String::new();
    if !vision {
        t.push_str("(No image is attached - work only from the file details below and be conservative.)\n");
    } else if c.kind == "video" {
        t.push_str("The image is one frame from a video.\n");
    }
    t.push_str(&format!("File: {}\nFolder: {}\n", c.name, c.folder));
    if !c.taken_at.is_empty() {
        t.push_str(&format!("Date taken: {} (from {})\n", c.taken_at, c.date_source));
    }
    if !c.camera.is_empty() {
        t.push_str(&format!("Camera: {}\n", c.camera));
    }
    t.push_str(&format!(
        "Already tagged: {}\n",
        if c.tags.is_empty() { "nothing".to_string() } else { c.tags.join(", ") }
    ));
    if !c.notes.trim().is_empty() {
        t.push_str(&format!("Owner's notes: {}\n", c.notes.chars().take(400).collect::<String>()));
    }
    t
}

/// A parsed review, already de-duplicated against what the item has.
#[derive(Debug, Default, PartialEq)]
pub struct Review {
    pub description: String,
    pub tags: Vec<(String, String)>, // (category, name)
    pub note: String,
}

pub fn parse_review(v: &Value, existing_tags: &[String], max_tags: usize) -> Review {
    let clean = |s: &str| s.trim().trim_matches(['"', '#', '.']).trim().chars().take(60).collect::<String>();
    let mut r = Review {
        description: v["description"].as_str().map(|s| s.trim().chars().take(1000).collect()).unwrap_or_default(),
        note: v["note"].as_str().or_else(|| v["notes"].as_str()).map(|s| s.trim().chars().take(1000).collect()).unwrap_or_default(),
        ..Default::default()
    };
    let have: std::collections::HashSet<String> = existing_tags
        .iter()
        .map(|t| t.rsplit(':').next().unwrap_or(t).trim().to_lowercase())
        .collect();
    let mut seen = std::collections::HashSet::new();
    if let Some(arr) = v["tags"].as_array() {
        for t in arr {
            let (cat, name) = match t {
                Value::String(s) => match s.split_once(':') {
                    Some((c, n)) => (clean(c), clean(n)),
                    None => (String::new(), clean(s)),
                },
                Value::Object(_) => (
                    clean(t["category"].as_str().unwrap_or("")),
                    clean(t["name"].as_str().or_else(|| t["tag"].as_str()).unwrap_or("")),
                ),
                _ => continue,
            };
            if name.is_empty() || name.len() < 2 {
                continue;
            }
            let k = name.to_lowercase();
            if have.contains(&k) || !seen.insert(k) {
                continue;
            }
            r.tags.push((cat, name));
            if r.tags.len() >= max_tags {
                break;
            }
        }
    }
    let n = r.note.to_lowercase();
    if n == "none" || n == "n/a" || n == "\"\"" {
        r.note.clear();
    }
    r
}

fn add_suggestion(conn: &Connection, job: i64, media: i64, kind: &str, payload: Value) {
    let _ = conn.execute(
        "INSERT INTO ai_suggestions (job_id, media_id, kind, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![job, media, kind, payload.to_string(), now()],
    );
}

fn tag_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row("SELECT 1 FROM tags WHERE name = ?1", [name], |_| Ok(())).is_ok()
}

/// Review one item: ask the model, store its raw reply, queue suggestions.
fn review_one(conn: &Connection, s: &Settings, system: &str, job: i64, media: i64) -> Result<usize, String> {
    let ctx = item_ctx(conn, media)?;
    let img = if s.vision { Some(prepare_image(conn, media, s.image_max_px)?) } else { None };
    let t0 = Instant::now();
    let reply = chat(s, system, &review_user_text(&ctx, s.vision), img.as_ref());
    let ms = t0.elapsed().as_millis() as i64;
    let _ = conn.execute(
        "INSERT INTO ai_responses (job_id, media_id, at, ms, ok, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![job, media, now(), ms, reply.is_ok() as i64, reply.as_ref().map(|t| t.clone()).unwrap_or_else(|e| e.clone())],
    );
    let reply = reply?;
    let v = extract_json(&reply).ok_or_else(|| {
        format!("model did not return JSON: {}", reply.chars().take(160).collect::<String>())
    })?;
    let r = parse_review(&v, &ctx.tags, s.max_tags.clamp(1, 30) as usize);
    // Drop earlier still-pending suggestions for this item (re-review replaces them).
    let _ = conn.execute(
        "UPDATE ai_suggestions SET status = 'rejected', decided_at = ?2, error = 'superseded by a newer review'
         WHERE media_id = ?1 AND status = 'pending'",
        params![media, now()],
    );
    let mut n = 0;
    if !r.description.is_empty() && r.description != ctx.description {
        add_suggestion(conn, job, media, "description", json!({ "text": r.description, "before": ctx.description }));
        n += 1;
    }
    for (cat, name) in &r.tags {
        add_suggestion(conn, job, media, "add_tag", json!({ "category": cat, "name": name, "isNew": !tag_exists(conn, name) }));
        n += 1;
    }
    if !r.note.is_empty() && !ctx.notes.to_lowercase().contains(&r.note.to_lowercase()) {
        add_suggestion(conn, job, media, "append_note", json!({ "text": r.note }));
        n += 1;
    }
    let _ = conn.execute("INSERT OR IGNORE INTO annotations (media_id, updated_at) VALUES (?1, ?2)", params![media, now()]);
    let _ = conn.execute("UPDATE annotations SET ai_reviewed_at = ?2 WHERE media_id = ?1", params![media, now()]);
    Ok(n)
}

// ---------------------------------------------------------------------------
// Jobs (background, one at a time)
// ---------------------------------------------------------------------------

static RUNNER: Mutex<()> = Mutex::new(());

fn create_job(conn: &Connection, kind: &str, label: &str, model: &str, ids: &[i64], status: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO ai_jobs (created_at, kind, label, model, total, status, ids) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![now(), kind, label, model, ids.len() as i64, status, serde_json::to_string(ids).unwrap_or_default()],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn start_review(conn: &Connection, ids: &[i64], label: &str, rereview: bool) -> Result<i64, String> {
    let s = load_settings(conn);
    if s.model.trim().is_empty() {
        return Err("set up the AI connection and pick a model first (AI tab → Settings)".into());
    }
    if ids.is_empty() {
        return Err("nothing to review".into());
    }
    // Mark this process as the job runner BEFORE the job exists, so the
    // "jobs left running by a previous app session" cleanup in db::open never
    // cancels a job this process just queued.
    conn.execute(
        "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('ai_runner_pid', ?1)",
        [std::process::id().to_string()],
    )
    .map_err(|e| e.to_string())?;
    let job = create_job(conn, "review", label, &s.model, ids, "queued")?;
    let ids = ids.to_vec();
    std::thread::spawn(move || run_review(job, ids, rereview));
    Ok(job)
}

fn job_status(conn: &Connection, job: i64) -> String {
    conn.query_row("SELECT status FROM ai_jobs WHERE id = ?1", [job], |r| r.get(0)).unwrap_or_default()
}

fn run_review(job: i64, ids: Vec<i64>, rereview: bool) {
    let _guard = RUNNER.lock().unwrap_or_else(|p| p.into_inner());
    let conn = crate::db::open(&crate::paths::db_path());
    let _ = conn.execute(
        "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('ai_runner_pid', ?1)",
        [std::process::id().to_string()],
    );
    if job_status(&conn, job) == "cancelled" {
        return;
    }
    let s = load_settings(&conn); // settings at start of the run
    let system = review_system_prompt(&s, &vocabulary(&conn, 250));
    let _ = conn.execute("UPDATE ai_jobs SET status = 'running', model = ?2 WHERE id = ?1", params![job, s.model]);
    let (mut done, mut failed) = (0i64, 0i64);
    let mut last_err: Option<String> = None;
    let mut consecutive_fail = 0;
    for (i, id) in ids.iter().enumerate() {
        if job_status(&conn, job) == "cancelled" {
            break;
        }
        let reviewed: bool = conn
            .query_row("SELECT ai_reviewed_at IS NOT NULL FROM annotations WHERE media_id = ?1", [id], |r| r.get(0))
            .unwrap_or(false);
        if reviewed && !rereview {
            done += 1;
        } else {
            match review_one(&conn, &s, &system, job, *id) {
                Ok(_) => {
                    done += 1;
                    consecutive_fail = 0;
                }
                Err(e) => {
                    failed += 1;
                    consecutive_fail += 1;
                    last_err = Some(e);
                }
            }
        }
        let _ = conn.execute(
            "UPDATE ai_jobs SET done = ?2, failed = ?3, error = ?4 WHERE id = ?1",
            params![job, done, failed, last_err],
        );
        // Server down / wrong model: stop instead of failing 10,000 times.
        if consecutive_fail >= 5 && done == 0 && i >= 4 {
            let _ = conn.execute(
                "UPDATE ai_jobs SET status = 'error', finished_at = ?2 WHERE id = ?1",
                params![job, now()],
            );
            return;
        }
    }
    let _ = conn.execute(
        "UPDATE ai_jobs SET status = CASE WHEN status = 'cancelled' THEN 'cancelled' ELSE 'done' END, finished_at = ?2 WHERE id = ?1",
        params![job, now()],
    );
}

fn list_jobs(conn: &Connection, limit: i64) -> Value {
    let rows: Vec<Value> = conn
        .prepare(
            "SELECT j.id, j.created_at, j.kind, j.label, j.model, j.total, j.done, j.failed, j.status, j.error, j.finished_at,
                    (SELECT COUNT(*) FROM ai_suggestions s WHERE s.job_id = j.id AND s.status = 'pending'),
                    (SELECT COUNT(*) FROM ai_suggestions s WHERE s.job_id = j.id AND s.status = 'applied'),
                    (SELECT COUNT(*) FROM ai_suggestions s WHERE s.job_id = j.id AND s.status = 'rejected'),
                    (SELECT COUNT(*) FROM change_log c WHERE c.job_id = j.id AND c.undone_at IS NULL)
             FROM ai_jobs j ORDER BY j.id DESC LIMIT ?1",
        )
        .and_then(|mut st| {
            st.query_map([limit], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "createdAt": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
                    "label": r.get::<_, String>(3)?, "model": r.get::<_, String>(4)?, "total": r.get::<_, i64>(5)?,
                    "done": r.get::<_, i64>(6)?, "failed": r.get::<_, i64>(7)?, "status": r.get::<_, String>(8)?,
                    "error": r.get::<_, Option<String>>(9)?, "finishedAt": r.get::<_, Option<i64>>(10)?,
                    "pending": r.get::<_, i64>(11)?, "applied": r.get::<_, i64>(12)?, "rejected": r.get::<_, i64>(13)?,
                    "liveChanges": r.get::<_, i64>(14)?,
                }))
            })
            .map(|it| it.flatten().collect())
        })
        .unwrap_or_default();
    Value::Array(rows)
}

// ---------------------------------------------------------------------------
// Approval queue
// ---------------------------------------------------------------------------

fn sugg_json(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let p: String = r.get(4)?;
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "jobId": r.get::<_, i64>(1)?, "mediaId": r.get::<_, i64>(2)?,
        "kind": r.get::<_, String>(3)?, "payload": serde_json::from_str::<Value>(&p).unwrap_or(Value::Null),
        "status": r.get::<_, String>(5)?, "createdAt": r.get::<_, i64>(6)?, "error": r.get::<_, Option<String>>(7)?,
    }))
}
const SUGG_COLS: &str = "id, job_id, media_id, kind, payload, status, created_at, error";

/// Pending suggestions grouped by photo (one card per photo).
fn queue(conn: &Connection, a: &Value) -> Result<Value, String> {
    let status = a["status"].as_str().unwrap_or("pending").to_string();
    let job = a["jobId"].as_i64();
    let limit = a["limit"].as_i64().unwrap_or(60).clamp(1, 500);
    let offset = a["offset"].as_i64().unwrap_or(0).max(0);
    let (where_sql, p): (String, Vec<rusqlite::types::Value>) = match job {
        Some(j) => ("status = ?1 AND job_id = ?2".into(), vec![status.clone().into(), j.into()]),
        None => ("status = ?1".into(), vec![status.clone().into()]),
    };
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(DISTINCT media_id) FROM ai_suggestions WHERE {where_sql}"),
            rusqlite::params_from_iter(p.iter()),
            |r| r.get(0),
        )
        .unwrap_or(0);
    let total_sugg: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM ai_suggestions WHERE {where_sql}"),
            rusqlite::params_from_iter(p.iter()),
            |r| r.get(0),
        )
        .unwrap_or(0);
    let mut pp = p.clone();
    pp.push(limit.into());
    pp.push(offset.into());
    let n = p.len();
    let media: Vec<i64> = conn
        .prepare(&format!(
            "SELECT media_id FROM ai_suggestions WHERE {where_sql} GROUP BY media_id ORDER BY MAX(id) DESC LIMIT ?{} OFFSET ?{}",
            n + 1,
            n + 2
        ))
        .and_then(|mut st| st.query_map(rusqlite::params_from_iter(pp.iter()), |r| r.get(0)).map(|it| it.flatten().collect()))
        .map_err(|e| e.to_string())?;
    let f = crate::query::Filter { ids: Some(media.clone()), presence: Some("all".into()), ..Default::default() };
    let items = crate::query::apply_filter(conn, &f)?;
    let by_id: std::collections::HashMap<i64, &crate::query::Item> = items.iter().map(|i| (i.id, i)).collect();
    let mut cards = vec![];
    for m in media {
        let suggestions: Vec<Value> = conn
            .prepare(&format!(
                "SELECT {SUGG_COLS} FROM ai_suggestions WHERE media_id = ?1 AND status = ?2 {} ORDER BY id",
                if job.is_some() { "AND job_id = ?3" } else { "" }
            ))
            .and_then(|mut st| match job {
                Some(j) => st.query_map(params![m, status, j], sugg_json).map(|it| it.flatten().collect()),
                None => st.query_map(params![m, status], sugg_json).map(|it| it.flatten().collect()),
            })
            .unwrap_or_default();
        let ann = crate::library::annotation(conn, m);
        cards.push(json!({
            "mediaId": m,
            "item": by_id.get(&m).map(|i| serde_json::to_value(i).unwrap_or(Value::Null)),
            "current": { "tags": ann["tags"], "notes": ann["notes"] },
            "suggestions": suggestions,
        }));
    }
    Ok(json!({ "totalPhotos": total, "totalSuggestions": total_sugg, "cards": cards }))
}

/// Apply one approved suggestion through the normal library functions so it
/// lands in change_log (source = ai) and can be undone.
fn apply_one(conn: &mut Connection, sid: i64, override_payload: Option<&Value>) -> Result<(), String> {
    let (job, media, kind, payload, status): (i64, i64, String, String, String) = conn
        .query_row(
            "SELECT job_id, media_id, kind, payload, status FROM ai_suggestions WHERE id = ?1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|_| format!("suggestion {sid} not found"))?;
    if status != "pending" && status != "undone" {
        return Ok(()); // already decided
    }
    let p: Value = match override_payload {
        Some(o) => o.clone(),
        None => serde_json::from_str(&payload).unwrap_or(Value::Null),
    };
    let s = |k: &str| p[k].as_str().unwrap_or("").trim().to_string();
    let res: Result<(), String> = history::with_source(Source::ai(job, sid), || match kind.as_str() {
        "add_tag" => {
            let (cat, name) = (s("category"), s("name"));
            if name.is_empty() {
                return Err("empty tag".into());
            }
            // Never re-categorize a tag the user already has.
            let label = if cat.is_empty() || tag_exists(conn, &name) { name } else { format!("{cat}: {name}") };
            crate::library::add_tags(conn, &[media], &[label], None).map(|_| ())
        }
        "remove_tag" => {
            let name = s("name");
            let tid: Option<i64> = conn.query_row("SELECT id FROM tags WHERE name = ?1", [&name], |r| r.get(0)).ok();
            match tid {
                Some(t) => crate::library::remove_tags(conn, &[media], &[t]).map(|_| ()),
                None => Ok(()),
            }
        }
        "description" => crate::library::set_description(conn, media, &s("text")),
        "append_note" => crate::library::append_notes(conn, &[media], &s("text")).map(|_| ()),
        "set_rating" => {
            let v = p["value"].as_i64().unwrap_or(0).clamp(0, 5);
            crate::library::set_field(conn, &[media], "rating", v.into()).map(|_| ())
        }
        "set_favorite" => {
            let v = p["value"].as_bool().unwrap_or(true) as i64;
            crate::library::set_field(conn, &[media], "favorite", v.into()).map(|_| ())
        }
        "add_to_album" => {
            let a = crate::library::ensure_album(conn, &s("name"))?;
            crate::library::add_to_album(conn, a, &[media]).map(|_| ())
        }
        other => Err(format!("unknown suggestion kind {other}")),
    });
    match &res {
        Ok(()) => {
            let _ = conn.execute(
                "UPDATE ai_suggestions SET status = 'applied', decided_at = ?2, error = NULL, payload = ?3 WHERE id = ?1",
                params![sid, now(), p.to_string()],
            );
        }
        Err(e) => {
            let _ = conn.execute(
                "UPDATE ai_suggestions SET status = 'failed', decided_at = ?2, error = ?3 WHERE id = ?1",
                params![sid, now(), e],
            );
        }
    }
    res
}

fn ids_of(conn: &Connection, sql: &str, p: Vec<rusqlite::types::Value>) -> Vec<i64> {
    conn.prepare(sql)
        .and_then(|mut st| st.query_map(rusqlite::params_from_iter(p.iter()), |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default()
}

/// Which pending suggestions an approve/reject call targets.
fn target_suggestions(conn: &Connection, a: &Value) -> Vec<i64> {
    if let Some(list) = a["suggestionIds"].as_array() {
        return list.iter().filter_map(|v| v.as_i64()).collect();
    }
    if let Some(list) = a["mediaIds"].as_array() {
        let ids: Vec<String> = list.iter().filter_map(|v| v.as_i64()).map(|i| i.to_string()).collect();
        if ids.is_empty() {
            return vec![];
        }
        let job_clause = a["jobId"].as_i64().map(|j| format!(" AND job_id = {j}")).unwrap_or_default();
        return ids_of(
            conn,
            &format!("SELECT id FROM ai_suggestions WHERE status = 'pending' AND media_id IN ({}){job_clause} ORDER BY id", ids.join(",")),
            vec![],
        );
    }
    if let Some(j) = a["jobId"].as_i64() {
        return ids_of(conn, "SELECT id FROM ai_suggestions WHERE status = 'pending' AND job_id = ?1 ORDER BY id", vec![j.into()]);
    }
    if a["all"].as_bool() == Some(true) {
        return ids_of(conn, "SELECT id FROM ai_suggestions WHERE status = 'pending' ORDER BY id", vec![]);
    }
    vec![]
}

fn approve(conn: &mut Connection, a: &Value) -> Result<Value, String> {
    let ids = target_suggestions(conn, a);
    if ids.is_empty() {
        return Err("nothing to approve".into());
    }
    let edits = a["edits"].as_object().cloned().unwrap_or_default();
    let (mut ok, mut failed) = (0, vec![]);
    for sid in ids {
        let ov = edits.get(&sid.to_string());
        match apply_one(conn, sid, ov) {
            Ok(()) => ok += 1,
            Err(e) => failed.push(format!("#{sid}: {e}")),
        }
    }
    Ok(json!({ "applied": ok, "failed": failed }))
}

fn reject(conn: &Connection, a: &Value) -> Result<Value, String> {
    let ids = target_suggestions(conn, a);
    let mut n = 0;
    for sid in ids {
        n += conn
            .execute(
                "UPDATE ai_suggestions SET status = 'rejected', decided_at = ?2 WHERE id = ?1 AND status = 'pending'",
                params![sid, now()],
            )
            .unwrap_or(0);
    }
    Ok(json!({ "rejected": n }))
}

// ---------------------------------------------------------------------------
// Assistant ("Ask"): natural language -> filter / review / proposed changes
// ---------------------------------------------------------------------------

fn assistant_system(conn: &Connection, ctx: &Value) -> String {
    let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
    let on_disk = "EXISTS (SELECT 1 FROM occurrence o WHERE o.media_id = m.id AND o.missing = 0)";
    let photos = q(&format!("SELECT COUNT(*) FROM media m WHERE kind = 'image' AND {on_disk}"));
    let videos = q(&format!("SELECT COUNT(*) FROM media m WHERE kind = 'video' AND {on_disk}"));
    let reviewed = q("SELECT COUNT(*) FROM annotations WHERE ai_reviewed_at IS NOT NULL");
    let years: String = conn
        .prepare("SELECT substr(taken_at,1,4) y, COUNT(*) FROM media WHERE taken_at IS NOT NULL GROUP BY y ORDER BY y")
        .and_then(|mut st| {
            st.query_map([], |r| Ok(format!("{} ({})", r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                .map(|it| it.flatten().collect::<Vec<_>>().join(", "))
        })
        .unwrap_or_default();
    let albums: String = conn
        .prepare("SELECT name FROM albums ORDER BY name")
        .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect::<Vec<_>>().join("; ")))
        .unwrap_or_default();
    let cats: String = conn
        .prepare("SELECT name FROM categories ORDER BY name")
        .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect::<Vec<_>>().join(", ")))
        .unwrap_or_default();
    format!(
        "You are the assistant inside photoApp, a photo and video library manager. You cannot see images here; \
you work from the catalog: file names, folders, dates, cameras, tags, notes and AI-written descriptions.\n\
Reply with ONLY one JSON object, no prose and no code fences:\n\
{{\"reply\": \"short helpful answer\", \"action\": null}}  or with one of these actions:\n\
1. {{\"type\": \"filter\", \"filter\": {{...}}}} to find or show items. Filter keys, all optional: \
search (keywords matched against file names, folders, notes, tags, categories, albums and AI descriptions; \
\"exact phrase\" and -exclude work), tags (array of tag names; item must have all), anyTags (array; item needs one), \
excludeTags (array), kind (\"image\" or \"video\"), dateFrom and dateTo (YYYY-MM-DD), year (YYYY), minRating (1-5), \
favorite (true), hasNotes (true), untagged (true), album (album name), camera (text), aiReviewed (true or false), sort \
(takenDesc, takenAsc, ratingDesc, sizeDesc, nameAsc).\n\
2. {{\"type\": \"review\", \"target\": \"selected\" or \"results\", \"rereview\": false}} to have the vision model look at \
photos and propose descriptions and tags.\n\
3. {{\"type\": \"propose\", \"target\": \"selected\" or \"results\", \"changes\": [...]}} where each change is one of \
{{\"type\": \"add_tag\", \"category\": \"...\", \"name\": \"...\"}}, {{\"type\": \"remove_tag\", \"name\": \"...\"}}, \
{{\"type\": \"append_note\", \"text\": \"...\"}}, {{\"type\": \"set_rating\", \"value\": 1-5}}, \
{{\"type\": \"set_favorite\", \"value\": true}}, {{\"type\": \"add_to_album\", \"name\": \"...\"}}. \
Proposed changes wait for the owner's approval; they are never applied directly.\n\
Use target \"selected\" for this / these / selected; \"results\" for everything currently shown. \
Prefer existing tag, category and album names. Search by keywords when no tag matches. \
Descriptions only exist for AI-reviewed items ({reviewed} so far); if a visual question can't be answered from metadata, \
say so and suggest an AI review.\n\
Library: {photos} photos, {videos} videos. Years: {years}.\n\
Categories: {cats}\nTags: {vocab}\nAlbums: {albums}\n\
Current view: {view} — {results} items shown, {selected} selected.",
        vocab = vocabulary(conn, 300),
        cats = if cats.is_empty() { "(none)".into() } else { cats },
        albums = if albums.is_empty() { "(none)".into() } else { albums },
        view = ctx["filter"].to_string(),
        results = ctx["resultIds"].as_array().map(|a| a.len()).unwrap_or(0),
        selected = ctx["selectedIds"].as_array().map(|a| a.len()).unwrap_or(0),
    )
}

fn tag_ids_for(conn: &Connection, names: &Value, unknown: &mut Vec<String>) -> Vec<i64> {
    let mut out = vec![];
    for n in names.as_array().cloned().unwrap_or_default() {
        let Some(raw) = n.as_str() else { continue };
        let name = raw.rsplit(':').next().unwrap_or(raw).trim();
        match conn.query_row("SELECT id FROM tags WHERE name = ?1", [name], |r| r.get::<_, i64>(0)) {
            Ok(id) => out.push(id),
            Err(_) => unknown.push(name.to_string()),
        }
    }
    out
}

/// Turn the model's filter (names) into the app's filter (ids).
fn resolve_filter(conn: &Connection, f: &Value) -> (Value, Vec<String>) {
    let mut out = json!({});
    let mut unknown = vec![];
    let copy = ["search", "kind", "dateFrom", "dateTo", "year", "minRating", "favorite", "hasNotes", "untagged", "camera", "aiReviewed", "sort"];
    for k in copy {
        if !f[k].is_null() {
            out[k] = f[k].clone();
        }
    }
    let all = tag_ids_for(conn, &f["tags"], &mut unknown);
    let any = tag_ids_for(conn, &f["anyTags"], &mut unknown);
    if !any.is_empty() && all.is_empty() {
        out["tags"] = json!(any);
        out["tagMode"] = json!("any");
    } else if !all.is_empty() {
        out["tags"] = json!(all);
    }
    let ex = tag_ids_for(conn, &f["excludeTags"], &mut unknown);
    if !ex.is_empty() {
        out["excludeTags"] = json!(ex);
    }
    if let Some(a) = f["album"].as_str() {
        match conn.query_row("SELECT id FROM albums WHERE name = ?1", [a], |r| r.get::<_, i64>(0)) {
            Ok(id) => out["album"] = json!(id),
            Err(_) => unknown.push(format!("album {a}")),
        }
    }
    // Unknown tag names still help as search words.
    if !unknown.is_empty() && out["search"].is_null() {
        let words: Vec<String> = unknown.iter().filter(|u| !u.starts_with("album ")).cloned().collect();
        if !words.is_empty() {
            out["search"] = json!(words.join(" "));
        }
    }
    (out, unknown)
}

fn ask(conn: &mut Connection, a: &Value) -> Result<Value, String> {
    let s = load_settings(conn);
    let question = a["question"].as_str().unwrap_or("").trim().to_string();
    if question.is_empty() {
        return Err("ask a question".into());
    }
    let system = assistant_system(conn, a);
    let t0 = Instant::now();
    // The assistant only needs text; no JSON mode trouble on small models.
    let reply = chat(&s, &system, &question, None)?;
    let job_audit = create_job(conn, "assistant", &question.chars().take(120).collect::<String>(), &s.model, &[], "done")?;
    let _ = conn.execute(
        "INSERT INTO ai_responses (job_id, media_id, at, ms, ok, text) VALUES (?1, 0, ?2, ?3, 1, ?4)",
        params![job_audit, now(), t0.elapsed().as_millis() as i64, reply],
    );
    let Some(v) = extract_json(&reply) else {
        return Ok(json!({ "reply": reply.trim(), "action": null, "jobId": job_audit }));
    };
    let text = v["reply"].as_str().unwrap_or("").to_string();
    let action = &v["action"];
    let target_ids = |t: &str| -> Vec<i64> {
        let key = if t == "selected" { "selectedIds" } else { "resultIds" };
        let mut ids: Vec<i64> = a[key].as_array().map(|x| x.iter().filter_map(|i| i.as_i64()).collect()).unwrap_or_default();
        if ids.is_empty() && t == "selected" {
            ids = a["resultIds"].as_array().map(|x| x.iter().filter_map(|i| i.as_i64()).collect()).unwrap_or_default();
        }
        ids.truncate(5000);
        ids
    };
    match action["type"].as_str() {
        Some("filter") => {
            let (filter, unknown) = resolve_filter(conn, &action["filter"]);
            Ok(json!({ "reply": text, "action": "filter", "filter": filter, "unknown": unknown, "jobId": job_audit }))
        }
        Some("review") => {
            let ids = target_ids(action["target"].as_str().unwrap_or("selected"));
            if ids.is_empty() {
                return Ok(json!({ "reply": format!("{text} (Nothing to review — select photos first.)"), "action": null }));
            }
            let job = start_review(conn, &ids, &format!("Ask: {}", question.chars().take(80).collect::<String>()), action["rereview"].as_bool().unwrap_or(false))?;
            Ok(json!({ "reply": text, "action": "review", "jobId": job, "count": ids.len() }))
        }
        Some("propose") => {
            let ids = target_ids(action["target"].as_str().unwrap_or("selected"));
            let changes = action["changes"].as_array().cloned().unwrap_or_default();
            if ids.is_empty() || changes.is_empty() {
                return Ok(json!({ "reply": text, "action": null }));
            }
            let job = create_job(conn, "assistant", &question.chars().take(120).collect::<String>(), &s.model, &ids, "done")?;
            let mut n = 0;
            for m in &ids {
                for c in &changes {
                    let kind = c["type"].as_str().unwrap_or("");
                    if !matches!(kind, "add_tag" | "remove_tag" | "append_note" | "set_rating" | "set_favorite" | "add_to_album") {
                        continue;
                    }
                    let mut p = c.clone();
                    if let Some(o) = p.as_object_mut() {
                        o.remove("type");
                    }
                    if kind == "add_tag" {
                        let nm = p["name"].as_str().unwrap_or("").to_string();
                        p["isNew"] = json!(!tag_exists(conn, &nm));
                    }
                    add_suggestion(conn, job, *m, kind, p);
                    n += 1;
                }
            }
            let _ = conn.execute("UPDATE ai_jobs SET done = total WHERE id = ?1", [job]);
            Ok(json!({ "reply": text, "action": "propose", "jobId": job, "count": n, "photos": ids.len() }))
        }
        _ => Ok(json!({ "reply": if text.is_empty() { reply.trim().to_string() } else { text }, "action": null, "jobId": job_audit })),
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

pub fn call(conn: &mut Connection, _cfg: &Config, cmd: &str, a: &Value) -> Result<Value, String> {
    match cmd {
        "ai.settings.get" => Ok(public_settings(&load_settings(conn))),
        "ai.settings.save" => save_settings(conn, a).map(|s| public_settings(&s)),
        "ai.models" => {
            let mut s = load_settings(conn);
            // Allow listing with unsaved form values.
            if let Some(u) = a["baseUrl"].as_str() { s.base_url = u.to_string(); }
            if let Some(p) = a["provider"].as_str() { s.provider = p.to_string(); }
            if let Some(k) = a["apiKey"].as_str().filter(|k| !k.is_empty()) { s.api_key = k.to_string(); }
            list_models(&s).map(|m| json!(m))
        }
        "ai.test" => {
            let s = load_settings(conn);
            let t0 = Instant::now();
            let models = list_models(&s).unwrap_or_default();
            let reply = chat(&s, "Reply with exactly: OK", "Say OK.", None)?;
            let mut out = json!({ "reply": reply.trim().chars().take(200).collect::<String>(), "ms": t0.elapsed().as_millis() as i64, "models": models.len() });
            // Vision check on one real photo, when there is one.
            if s.vision {
                let any: Option<i64> = conn
                    .query_row(
                        "SELECT m.id FROM media m JOIN occurrence o ON o.media_id = m.id AND o.missing = 0 WHERE m.kind = 'image' LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(id) = any {
                    match prepare_image(conn, id, s.image_max_px.min(512)).and_then(|img| {
                        chat(&s, "Describe the image in at most 12 words.", "What is in this picture?", Some(&img))
                    }) {
                        Ok(t) => out["vision"] = json!(t.trim().chars().take(200).collect::<String>()),
                        Err(e) => out["visionError"] = json!(e),
                    }
                }
            }
            Ok(out)
        }
        "ai.review" => {
            let ids: Vec<i64> = a["ids"].as_array().map(|x| x.iter().filter_map(|i| i.as_i64()).collect()).unwrap_or_default();
            let label = a["label"].as_str().unwrap_or("Review").to_string();
            let job = start_review(conn, &ids, &label, a["rereview"].as_bool().unwrap_or(false))?;
            Ok(json!({ "jobId": job, "count": ids.len() }))
        }
        "ai.jobs" => Ok(list_jobs(conn, a["limit"].as_i64().unwrap_or(30).clamp(1, 500))),
        "ai.cancel" => {
            let j = a["jobId"].as_i64().ok_or("missing jobId")?;
            conn.execute(
                "UPDATE ai_jobs SET status = 'cancelled', finished_at = ?2 WHERE id = ?1 AND status IN ('queued','running')",
                params![j, now()],
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        "ai.queue" => queue(conn, a),
        "ai.forMedia" => {
            let m = a["id"].as_i64().ok_or("missing id")?;
            let rows: Vec<Value> = conn
                .prepare(&format!("SELECT {SUGG_COLS} FROM ai_suggestions WHERE media_id = ?1 AND status = 'pending' ORDER BY id"))
                .and_then(|mut st| st.query_map([m], sugg_json).map(|it| it.flatten().collect()))
                .unwrap_or_default();
            Ok(Value::Array(rows))
        }
        "ai.approve" => approve(conn, a),
        "ai.reject" => reject(conn, a),
        "ai.ask" => ask(conn, a),
        "ai.lastResponse" => {
            let m = a["id"].as_i64().ok_or("missing id")?;
            let r: Option<(i64, i64, String)> = conn
                .query_row(
                    "SELECT at, ok, text FROM ai_responses WHERE media_id = ?1 ORDER BY id DESC LIMIT 1",
                    [m],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            Ok(r.map(|(at, ok, text)| json!({ "at": at, "ok": ok == 1, "text": text })).unwrap_or(Value::Null))
        }
        "ai.stats" => {
            let q = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
            Ok(json!({
                "pending": q("SELECT COUNT(*) FROM ai_suggestions WHERE status = 'pending'"),
                "pendingPhotos": q("SELECT COUNT(DISTINCT media_id) FROM ai_suggestions WHERE status = 'pending'"),
                "reviewed": q("SELECT COUNT(*) FROM annotations WHERE ai_reviewed_at IS NOT NULL"),
                "running": q("SELECT COUNT(*) FROM ai_jobs WHERE status IN ('queued','running')"),
                "configured": !load_settings(conn).model.trim().is_empty(),
            }))
        }
        _ => Err(format!("unknown command: {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_variants() {
        let v = extract_json("<think>hmm {not json}</think>\n```json\n{\"a\": 1, \"b\": \"x}\"}\n```").unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "x}");
        assert!(extract_json("no json here").is_none());
        assert_eq!(extract_json("Sure! {\"reply\":\"hi\",\"action\":null} ok").unwrap()["reply"], "hi");
    }

    #[test]
    fn parse_review_dedupes_and_caps() {
        let v = json!({
            "description": "A white race boat on blue water.",
            "tags": [{"category":"Vehicle","name":"Race Boat"}, "Place: Key West", "race boat", {"category":"Subject","name":"Water"}, "Dog", "#Sunset."],
            "note": "none"
        });
        let r = parse_review(&v, &["Subject: Water".to_string()], 3);
        assert_eq!(r.description, "A white race boat on blue water.");
        assert_eq!(r.tags, vec![
            ("Vehicle".into(), "Race Boat".into()),
            ("Place".into(), "Key West".into()),
            ("".into(), "Dog".into()),
        ]);
        assert_eq!(r.note, "");
    }
}
