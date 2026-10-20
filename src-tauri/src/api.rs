//! Dev/standalone HTTP API: a zero-dependency (std TcpListener) JSON server
//! that exposes the same catalog queries the Tauri app uses. The Svelte dev
//! server in a plain browser talks to this; the packaged app uses IPC.
//!
//!   photoapp.exe api [--port 8787]
//!     GET  /api/catalog[?kind=&year=&search=...]  -> Vec<Item>
//!     GET  /api/item/<id>                          -> Detail
//!     GET  /api/stats                              -> stats json
//!     GET  /api/facets                             -> Facets
//!     GET  /api/dups[?limit=N]                      -> Vec<DupGroup>   (M7)
//!     GET  /api/dups/summary                        -> DupSummary      (M7)
//!     POST /api/dups/<mediaId>/dry-run              -> StagingPlan     (M7)
//!     POST /api/dups/<mediaId>/apply                -> StagingPlan     (M7)
//!     POST /api/dups/<mediaId>/undo                 -> StagingPlan     (M7)
//!     POST /api/call/<cmd>   {json args}            -> library::call (tags, notes,
//!                                                      albums, bulk ops, maintenance)
//!
//! Binds 127.0.0.1 only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use crate::{
    ops,
    query::{self, Filter},
    scanner,
};

fn load_cfg() -> scanner::Config {
    let p = crate::paths::config_path();
    scanner::load_config(std::path::Path::new(&p))
}

fn open_db() -> rusqlite::Connection {
    let p = crate::paths::db_path();
    crate::db::open(p.as_str())
}

pub fn serve(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind api port");
    println!("api: listening on http://127.0.0.1:{port}");
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            std::thread::spawn(move || {
                let _ = handle(stream);
            });
        }
    }
}

fn handle(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_millis(2000))).ok();
    let mut req: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];

    // Phase 1: read until the header terminator (headers + blank line).
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(e) => return Err(e),
        }
    }

    // Determine the body length from the Content-Length header (if any).
    let head_end = req.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    let (head_bytes, body_start) = match head_end {
        Some(i) => (&req[..i], i),
        None => (req.as_slice(), req.len()),
    };
    let head_str = String::from_utf8_lossy(head_bytes);
    let content_length: usize = head_str
        .lines()
        .find_map(|h| {
            h.split_once(':')
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim()))
        })
        .filter(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    // Phase 2: read the remainder of the body (Content-Length bytes past the
    // header terminator). Short reads are normal, so loop until satisfied.
    let have_body = req.len() - body_start;
    let want_body = content_length;
    if want_body > have_body {
        loop {
            let remaining = want_body - (req.len() - body_start);
            if remaining == 0 {
                break;
            }
            match stream.read(&mut buf) {
                Ok(0) => break, // peer closed
                Ok(n) => req.extend_from_slice(&buf[..n.min(remaining as usize)]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => return Err(e),
            }
        }
    }

    let req = String::from_utf8_lossy(&req).to_string();
    let (head, body) = match req.find("\r\n\r\n") {
        Some(i) => (req[..i].to_string(), req[i + 4..].to_string()),
        None => (req.clone(), String::new()),
    };
    let line = head.lines().next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    let (path, _qs) = match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i + 1..].to_string()),
        None => (target.to_string(), String::new()),
    };

    match (method, path.as_str()) {
        ("GET", p) if p.starts_with("/api/") => {
            let qs = target
                .find('?')
                .map(|i| &target[i + 1..])
                .unwrap_or("")
                .to_string();
            let filter = parse_filter(&qs);
            let body: (u16, String) = if p.starts_with("/api/item/") {
                let id: i64 = p
                    .trim_start_matches("/api/item/")
                    .trim_end_matches('/')
                    .parse()
                    .unwrap_or(-1);
                let conn = open_db();
                match query::item_detail(&conn, id) {
                    Ok(d) => (200, serde_json::to_string(&d).unwrap_or_default()),
                    Err(e) => (404, json_err(&e)),
                }
            } else if p == "/api/catalog" || p == "/api/query" {
                let conn = open_db();
                match query::apply_filter(&conn, &filter) {
                    Ok(items) => (200, serde_json::to_string(&items).unwrap_or_default()),
                    Err(e) => (500, json_err(&e)),
                }
            } else if p == "/api/stats" {
                let conn = open_db();
                let s = query::stats(&conn);
                (200, s.to_string())
            } else if p == "/api/facets" {
                let conn = open_db();
                let cfg = load_cfg();
                let f = query::facets(&conn, &cfg);
                (200, serde_json::to_string(&f).unwrap_or_default())
            } else if p == "/api/dups" {
                let limit = filter.limit.map(|n| n as i64);
                let conn = open_db();
                match ops::dup_groups(&conn, limit) {
                    Ok(g) => (200, serde_json::to_string(&g).unwrap_or_default()),
                    Err(e) => (500, json_err(&e)),
                }
            } else if p == "/api/dups/summary" {
                let conn = open_db();
                (200, serde_json::to_string(&ops::dup_summary(&conn)).unwrap_or_default())
            } else if p.starts_with("/api/relocate/") && p.ends_with("/move-target") {
                // GET /api/relocate/<mediaId>/move-target?layout=year|year_month|root
                let rest = p
                    .trim_start_matches("/api/relocate/")
                    .trim_end_matches("/move-target");
                let id: i64 = match rest.parse() {
                    Ok(v) => v,
                    Err(_) => return respond(&mut stream, 400, "text/plain", b"bad media id"),
                };
                let layout = qs
                    .split('&')
                    .find_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), url_decode(v))))
                    .filter(|(k, _)| k == "layout")
                    .map(|(_, v)| v)
                    .unwrap_or_else(|| "year".to_string());
                let conn = open_db();
                let cfg = load_cfg();
                match ops::move_target_for(&conn, &cfg, id, &layout) {
                    Ok(t) => (200, serde_json::json!({ "target": t }).to_string()),
                    Err(e) => (400, json_err(&e)),
                }
            } else {
                return respond(&mut stream, 404, "text/plain", b"not found");
            };
            respond_json(&mut stream, body.0, &body.1)
        }
        ("POST", p) if p.starts_with("/api/call/") => {
            let cmd = url_decode(p.trim_start_matches("/api/call/").trim_end_matches('/'));
            let args: serde_json::Value = if body.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => return respond(&mut stream, 400, "application/json", br#"{"error":"invalid json body"}"#),
                }
            };
            let mut conn = open_db();
            let cfg = load_cfg();
            match crate::library::call(&mut conn, &cfg, &cmd, &args) {
                Ok(v) => {
                    // Browser-dev mode has no background thread: catalog a newly
                    // added / resumed folder right here.
                    if v.get("syncNeeded").and_then(|x| x.as_bool()) == Some(true) {
                        let cfg = load_cfg();
                        let _ = crate::library::sync(&mut conn, &cfg);
                        let _ = crate::dedupe::auto_resolve(&mut conn, &cfg);
                    }
                    respond_json(&mut stream, 200, &v.to_string())
                }
                Err(e) => respond_json(&mut stream, 400, &json_err(&e)),
            }
        }
        ("POST", p) if p.contains("/api/dups/") => {
            let rest = p
                .trim_start_matches("/api/dups/")
                .trim_end_matches('/');
            let (id_str, action) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, ""),
            };
            let id: i64 = match id_str.parse() {
                Ok(v) => v,
                Err(_) => return respond(&mut stream, 400, "text/plain", b"bad media id"),
            };
            let result = match action {
                "dry-run" => {
                    let conn = open_db();
                    let cfg = load_cfg();
                    ops::plan_staging(&conn, &cfg, id)
                }
                "apply" => {
                    let mut conn = open_db();
                    let cfg = load_cfg();
                    ops::apply_staging(&mut conn, &cfg, id)
                }
                "undo" => {
                    let mut conn = open_db();
                    ops::undo_staging(&mut conn, id)
                }
                _ => return respond(&mut stream, 404, "text/plain", b"unknown action"),
            };
            match result {
                Ok(plan) => respond_json(&mut stream, 200, &serde_json::to_string(&plan).unwrap_or_default()),
                Err(e) => respond_json(&mut stream, 400, &json_err(&e)),
            }
        }
        ("POST", p) if p.contains("/api/relocate/") => {
            // Body: { "mediaId": <int>, "op": "rename"|"move", "targetPath": "<abs path>" }
            let parsed: serde_json::Value = if body.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(&body) {
                    Ok(v) => v,
                    Err(_) => return respond(&mut stream, 400, "application/json", br#"{"error":"invalid json body"}"#),
                }
            };
            let id = match parsed.get("mediaId").and_then(|v| v.as_i64()) {
                Some(i) => i,
                None => return respond(&mut stream, 400, "application/json", br#"{"error":"missing mediaId"}"#),
            };
            let op = parsed
                .get("op")
                .and_then(|v| v.as_str())
                .unwrap_or("rename")
                .to_string();
            let action = p
                .trim_start_matches("/api/relocate/")
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            // `undo` only needs the mediaId; dry-run/apply need a target path.
            let target = if action == "undo" {
                String::new()
            } else {
                match parsed.get("targetPath").and_then(|v| v.as_str()) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => return respond(&mut stream, 400, "application/json", br#"{"error":"missing targetPath"}"#),
                }
            };
            let result = match action {
                "dry-run" => {
                    let conn = open_db();
                    ops::plan_relocate(&conn, id, &target, &op)
                }
                "apply" => {
                    let mut conn = open_db();
                    ops::apply_relocate(&mut conn, id, &target, &op)
                }
                "undo" => {
                    let mut conn = open_db();
                    ops::undo_relocate(&mut conn, id)
                }
                _ => return respond(&mut stream, 404, "application/json", br#"{"error":"unknown action"}"#),
            };
            match result {
                Ok(plan) => respond_json(&mut stream, 200, &serde_json::to_string(&plan).unwrap_or_default()),
                Err(e) => respond_json(&mut stream, 400, &json_err(&e)),
            }
        }
        _ => respond(&mut stream, 405, "text/plain", b"method not allowed"),
    }
}

fn json_err(e: &str) -> String {
    format!(
        "{{\"error\":{}}}",
        serde_json::to_string(e).unwrap_or_default()
    )
}

/// Parse a URL query string into a `Filter`. Tolerant: unknown keys ignored.
fn parse_filter(qs: &str) -> Filter {
    let mut f = Filter::default();
    for pair in qs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.find('=') {
            Some(i) => (pair[..i].to_string(), pair[i + 1..].to_string()),
            None => (pair.to_string(), String::new()),
        };
        let v = url_decode(&v);
        match k.as_str() {
            "kind" => f.kind = Some(v),
            "year" => f.year = Some(v),
            "decade" => f.decade = Some(v),
            "camera" => f.camera = Some(v),
            "folder" => f.folder = Some(v),
            "minWidth" => f.min_width = v.parse().ok(),
            "minHeight" => f.min_height = v.parse().ok(),
            "search" => f.search = Some(v),
            "sort" => f.sort = match v.as_str() {
                "nameAsc" => Some(crate::query::Sort::NameAsc),
                "sizeDesc" => Some(crate::query::Sort::SizeDesc),
                "takenAsc" => Some(crate::query::Sort::TakenAsc),
                "ratingDesc" => Some(crate::query::Sort::RatingDesc),
                "editedDesc" => Some(crate::query::Sort::EditedDesc),
                "similar" => Some(crate::query::Sort::Similar),
                _ => Some(crate::query::Sort::TakenDesc),
            },
            "limit" => f.limit = v.parse().ok(),
            // Library filters: the browser UI sends the full filter as JSON.
            "f" => {
                if let Ok(full) = serde_json::from_str::<Filter>(&v) {
                    f = full;
                }
            }
            _ => {}
        }
    }
    f
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
                let lo = (bytes[i + 2] as char).to_digit(16).unwrap_or(0) as u8;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nAccess-Control-Allow-Origin: *\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    respond(stream, status, "application/json", body.as_bytes())
}
