# M4/M5 Design (locked 2026-10-11)

Branch: `m4-m5-ui` (off `main` @ 2f29863, M0-M3 baseline).

## Architecture
- Keep the existing CLI binary (`scan` / `thumbs` / `watch`) AND add the Tauri 2
  GUI app in the same crate. `main.rs` dispatches on argv[1]:
  - `scan`, `thumbs` — existing behavior, unchanged.
  - `watch` — M4 event loop on stdout (testable headless).
  - `app` — launch the Tauri GUI (Tauri manages its own event loop).
  - no-arg — legacy scan (unchanged).
- Rust module split (src-tauri/src/):
  - `incremental.rs` (new) — `diff_scan(conn, cfg)`: walk roots vs DB →
    (new_files, missing_paths). Plus `process_file(conn, cfg, path) -> Option<media_id>`:
    hash + EXIF + upsert media/occurrence + thumb, for one file. Reuses scanner
    internals (refactor scanner to expose the per-file pipeline; full `scan`
    stays the bulk path).
  - `watcher.rs` (new) — `notify` recursive watcher on configured roots,
    500 ms debounce, dispatches Create→process_file, Remove→mark missing
    (occurrence.missing=1; media row kept). Public fn `run_watch(conn, cfg,
    handler: Fn(WatchEvent))` so both CLI (print) and Tauri (emit event)
    can drive it.
  - `tauri_app.rs` (new) — Tauri commands: `catalog_items()` → all media
    (compact fields, sorted taken_at DESC), `item_detail(id)` → full row +
    occurrences + dup group; watcher task started at app setup; emits
    `catalog-changed` after any DB change.
  - `db.rs` — add `media_fts`? NO, FTS is M6. Add `idx_occ_missing` if needed.
- Media to frontend:
  - Tauri mode: asset protocol (`convertFileSrc`) — scope `E:/Photos/**` and
    `C:/devops/photoApp/data/thumbs/**`. Grid thumbs, viewer full-res, and
    inline MP4 `<video>` all stream from disk; no IPC payload bloat.
  - Browser dev mode (Vite): tiny middleware plugin serves the same files at
    `/localfile/<urlencoded absolute path>`. Same engine (Chromium), so
    playback/rendering verification in a normal browser is representative of
    WebView2.
  - `mediaUrl(path)` helper picks between the two.
- Frontend (frontend/, Vite + Svelte 5):
  - Grid: virtualized. Fixed cell (thumb 156px + caption), columns computed
    from viewport width; only visible rows are DOM nodes (target < 80 nodes
    for 10,564 items). Absolute-positioned rows in a tall spacer div.
  - Viewer: full-screen overlay; image = full-res via mediaUrl; video =
    `<video controls autoplay>`; EXIF side panel (camera, dimensions, date,
    source, path, dup group). Keyboard: ←/→ navigate, Esc close.
  - Placeholder tile for items with no thumb (the 1 corrupted video).
  - On `catalog-changed` event (or poll fallback): refetch list, merge.
- Non-destructive: GUI never writes files; watcher only writes catalog.db +
  data/thumbs (app-owned state). E:\Photos is read-only to the app in M4/M5.

## Verification (evidence bar)
- M4: scratch root + scratch DB (NEVER E:\Photos for test churn):
  create file → row appears with EXIF + thumb; delete → missing=1;
  move (rename) → new path present.
- M5: `cargo tauri build` succeeds; frontend verified in Vite dev server:
  DOM node count with 10,564 items loaded, scroll through corpus, viewer
  opens with correct EXIF (cross-checked vs SQL), MP4 plays, placeholder for
  the corrupted video, screenshots saved.
- Lane reviews branch before any merge to main. No push.

## Out of scope here (later milestones)
M6 filters/FTS, M7 duplicates view, M8 ops engine, M9 packaging polish.
