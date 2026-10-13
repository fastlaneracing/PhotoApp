use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use rusqlite::Connection;

/// ffmpeg used for video thumbnails: PHOTOAPP_FFMPEG, else `ffmpeg = "..."` in
/// config.toml, else `ffmpeg` on PATH. Without ffmpeg, videos just get a
/// placeholder tile.
pub fn ffmpeg() -> &'static str {
    static F: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    F.get_or_init(|| {
        if let Ok(p) = std::env::var("PHOTOAPP_FFMPEG") {
            return p;
        }
        let cfg = crate::paths::config_path();
        std::fs::read_to_string(&cfg)
            .ok()
            .and_then(|t| toml::from_str::<crate::scanner::Config>(&t).ok())
            .and_then(|c| c.ffmpeg)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "ffmpeg".to_string())
    })
}

const MAX: u32 = 400;

struct Job {
    sha: String,
    kind: String,
    path: String,
    out: PathBuf,
}

pub struct Stats {
    pub jobs: usize,
    pub produced: usize,
    pub already: usize,
    pub images: usize,
    pub videos: usize,
    pub errors: Vec<(String, String)>,
}

// One thumbnail per UNIQUE CONTENT (keyed by sha256), not per path — duplicate
// paths share a content row, so they share one thumbnail. Resumable: an existing
// thumb file is skipped, and the DB is only written for thumbs that exist.
pub fn generate(conn: &mut Connection, out_dir: &Path) -> Stats {
    std::fs::create_dir_all(out_dir).unwrap();

    let mut jobs: Vec<Job> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT m.sha256, m.kind, COALESCE(\
                   (SELECT o.path FROM occurrence o WHERE o.media_id = m.id AND o.is_canonical = 1), \
                   (SELECT o.path FROM occurrence o WHERE o.media_id = m.id LIMIT 1)) \
                 FROM media m WHERE m.thumb_path IS NULL",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap();
        for r in rows.flatten() {
            let sha = r.0;
            jobs.push(Job {
                out: out_dir.join(format!("{}.jpg", sha)),
                sha,
                kind: r.1,
                path: r.2,
            });
        }
    }

    let results: Vec<(String, bool, bool, Option<String>)> = jobs
        .par_iter()
        .map(|j| {
            if j.out.exists() {
                return (j.sha.clone(), true, true, None);
            }
            let r = if j.kind == "image" {
                make_image(&j.path, &j.out)
            } else {
                make_video(&j.path, &j.out)
            };
            match r {
                Ok(()) => (j.sha.clone(), true, false, None),
                Err(e) => (j.sha.clone(), false, false, Some(e)),
            }
        })
        .collect();

    let mut stats = Stats {
        jobs: jobs.len(),
        produced: 0,
        already: 0,
        images: 0,
        videos: 0,
        errors: Vec::new(),
    };

    let by_sha_kind: HashMap<String, String> = jobs.iter().map(|j| (j.sha.clone(), j.kind.clone())).collect();

    let tx = conn.transaction().unwrap();
    for (sha, ok, was_existing, err) in results {
        if !ok {
            stats.errors.push((sha.clone(), err.unwrap_or_default()));
            continue;
        }
        if was_existing {
            stats.already += 1;
        } else {
            stats.produced += 1;
        }
        let k = by_sha_kind.get(&sha).cloned().unwrap_or_default();
        if k == "image" { stats.images += 1; } else { stats.videos += 1; }
        tx.execute(
            "UPDATE media SET thumb_path = ?1 WHERE sha256 = ?2",
            rusqlite::params![
                out_dir.join(format!("{}.jpg", sha)).to_string_lossy().to_string(),
                sha,
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    stats
}

// with_guessed_format(true): decode by CONTENT, not file extension. ~325 files
// in this corpus are JPEGs mislabeled as .png/.heic; extension-based decode
// fails on them with "Invalid PNG signature".
pub(crate) fn make_image(path: &str, out: &Path) -> Result<(), String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let th = img.thumbnail(MAX, MAX);
    th.save(out).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn make_video(path: &str, out: &Path) -> Result<(), String> {
    let outstr = out.to_string_lossy().to_string();
    let o = Command::new(ffmpeg())
        .args([
            "-v", "error", "-y", "-i", path, "-frames:v", "1", "-vf", "scale=400:-2", outstr.as_str(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() && out.exists() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}
