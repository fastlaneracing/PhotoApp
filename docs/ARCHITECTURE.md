# Architecture

photoApp is a [Tauri 2](https://tauri.app) desktop app: a Rust back end that owns the catalog and the file system, and a Svelte 5 front end in WebView2.

```
frontend/ (Svelte 5 + Vite)          src-tauri/ (Rust)
  App.svelte  ── invoke('call') ──►  tauri_app.rs  ─┐
  lib.js      ── /api (dev mode) ─►  api.rs        ─┤
                                                    ▼
                         library.rs  history.rs  ai.rs  query.rs  ops.rs
                                                    │
                     scanner.rs  incremental.rs  watcher.rs  thumbs.rs  exif.rs
                                                    │
                                         db.rs  →  data/catalog.db (SQLite, WAL, FTS5)
```

The same back-end code serves both the packaged app (Tauri commands) and a browser development mode (`photoapp.exe api` + `npm run dev`).

## Data model

Identity is **content**, not path:

| Table | Purpose |
|---|---|
| `media` | One row per unique file content (SHA-256): kind, size, dimensions, date taken (+ source: EXIF / Takeout sidecar / file name / file time), camera, thumbnail, `deleted_at`. |
| `occurrence` | One row per path where that content exists; `missing` when the file is gone, duplicate-group fields. |
| `annotations` | Per media: notes, rating, favorite, color label, AI description, `ai_reviewed_at`. |
| `tags`, `categories`, `media_tags` | Tags (optionally in a category) and their assignment. |
| `albums`, `album_items`, `saved_searches` | Albums and saved filter sets. |
| `change_log` | Every annotation change with before/after, source (`user` / `ai`), AI job and suggestion ids, `undone_at`. |
| `ai_jobs`, `ai_suggestions`, `ai_responses` | AI runs, proposed changes awaiting approval, raw model replies. |
| `op_journal` | File moves / duplicate staging, for undo. |
| `media_fts` | FTS5 index over path, camera, notes, tags, albums and AI description. |
| `app_meta` | Settings (AI connection, `dup_settings`), cached duplicate counts, one-time migration markers. |
| `dedupe_runs`, `dedupe_log`, `dup_ignore` | Duplicate clean-ups (one run = one undo), each file removed or moved, and groups marked "not duplicates". |

Because notes and tags hang off `media`, they survive renames and moves made anywhere. When a file is edited in place its content hash changes; the change is queued in `pending_carry` and, once every changed file has been seen, annotations are carried to the new content only if the old content is gone (a rename shuffle never mixes notes between photos).

## Keeping the catalog in sync

- **Launch sync** (background) and **Manage → Sync now**: walk the roots, diff against the catalog (new paths, changed mtimes, missing paths), process new/changed files, prune stale paths, rebuild duplicate groups. A root that is unreadable, or empty while the catalog says it has files, is treated as *offline* and left untouched.
- **Live watcher** (`notify`): per-file create / remove / rename events, and a full re-sync when a folder is created, moved or renamed (Windows reports those as a single folder event).
- **Name + date fallback** for files that were both moved and edited.

## Safety rules

These are invariants — please keep them:

1. Photo contents are never modified.
2. File moves are previewed, never overwrite, and are journaled for undo; cross-drive moves copy then remove.
3. Deletes go to the Windows Recycle Bin only, after verifying the drive has one; catalog rows are kept so a restore brings notes and tags back. Nothing is ever deleted or moved from a protected folder.
4. Removing catalog rows for vanished files backs up the catalog first.
5. The AI never writes directly: it creates `ai_suggestions`; applying one goes through the normal edit functions inside `history::with_source(Source::ai(..))`, so it lands in `change_log` and can be undone.
6. Schema migrations are additive and idempotent, run once per process inside `BEGIN IMMEDIATE`.

## Library folders and duplicates (`settings.rs`, `dedupe.rs`)

- **Folders** live in `config.toml` (`[[roots]]`, file order = priority, `enabled`, `protected`). `scanner::load_config` splits them into active `roots` and `paused`, so every walker and the watcher see active folders only; paused folders are treated like an offline drive by the sync. Settings rewrites the file (validated round-trip, `.bak` kept); the Tauri layer then widens the asset scope, signals the watcher to re-watch (`watcher::folders_changed`), and runs a background sync.
- **Fingerprints**: per image, `media.px_w/px_h` (real pixel size from the original's header, EXIF rotation applied) and `media.phash` (64-bit difference hash of the cached thumbnail, turned upright). Computed once per content after each sync.
- **Groups**: identical copies (one media row, several present paths) plus images whose fingerprints differ by ≤ 3 / 6 / 10 bits, have the same shape (±3 %) and, when both have a camera or file-name date, the same date. Every picture must be close to the group's keeper (no chaining). Groups are classified `exact`, `copies` (each other picture is clearly worse) or `similar` (look-alikes, review only).
- **Keeper**: quality (pixels → camera data → lossless → bytes) then original name then folder priority, or folder priority first.
- **Clean-up** (`dups.resolve`): re-validates against current groups, never touches protected folders, verifies the Recycle Bin per drive, journals every file in `dedupe_log`, merges annotations into the keeper under `history::Source { dedupe }` (change_log `job_id = -run`), and marks fully removed pictures deleted. `dups.undo` restores from the bin or the review folder, re-catalogs, and calls `history::undo_job(-run)`.

- **Similarity sort** (`query::order_by_similarity`, sort `similar`): the filtered items are regrouped in Rust after the SQL query. Images whose `phash` differ by ≤ `simThreshold` bits (default 10) are linked with union-find (chaining allowed — browsing only); groups are ordered largest first and each group is laid out as a greedy nearest-neighbour chain from its oldest picture. Items carry `simGroup` / `simSize` for the tile badge. Cost is O(n²) popcounts over the matching images, done in parallel blocks.

## AI pipeline (`ai.rs`)

1. **Settings** in `app_meta.ai_settings` (OpenAI-compatible `/chat/completions` or Anthropic `/v1/messages`). The API key is never returned to the UI.
2. **Review job** (background thread, one at a time): for each photo, decode the original (EXIF orientation applied), downsize, JPEG-encode, and send it with file name, folder, date, camera, current tags/notes, the owner's instructions and the most-used tag vocabulary. Videos and undecodable formats use the thumbnail.
3. **Parsing** is lenient (`<think>` blocks, code fences, prose, answers placed in a reasoning field). Tags are de-duplicated against the photo's existing tags.
4. **Suggestions** are stored as pending; the UI approves (optionally edited) or rejects them.
5. **Ask**: a text-only call that returns a filter, a review request, or proposed changes (queued for approval).

## Front end (`frontend/src`)

| File | Role |
|---|---|
| `App.svelte` | Layout, tabs, selection, keyboard shortcuts, refresh logic |
| `Sidebar.svelte`, `FilterBar.svelte` | Views, albums, category/tag tree (show / hide), filters |
| `Grid.svelte`, `Tile.svelte` | Virtualized grid |
| `Inspector.svelte`, `Annotator.svelte`, `BulkEditor.svelte`, `ItemAI.svelte` | Right panel: single item, bulk edit, AI suggestions + history |
| `Viewer.svelte` | Full-screen viewer, zoom/pan, rename/move |
| `AIView.svelte`, `AISettings.svelte`, `AskBar.svelte` | AI tab, settings, Ask box |
| `ManageView.svelte`, `DupsView.svelte`, `SettingsView.svelte` | Maintenance, tag/album management, duplicates, library folders + duplicate settings |
| `lib.js`, `store.svelte.js` | Back-end bridge (`call(cmd, args)`), shared state |

All library operations go through one command, `call(cmd, args)` → `library::call` (and `history::call` / `ai::call`), e.g. `tags.add`, `notes.set`, `bulk.moveApply`, `items.delete`, `ai.review`, `history.undoJob`, `folders.add`, `dups.scan`, `dups.resolve`, `dups.undo`.
