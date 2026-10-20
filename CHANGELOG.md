# Changelog

All notable changes to photoApp. Versions follow [Semantic Versioning](https://semver.org).

## [0.5.0] - 2026-10-18
### Added
- **Settings tab.**
  - **Library folders:** add folders or drives (folder picker or a typed path, e.g. a network share), rename, reorder by priority, pause, protect, or remove, and edit the skipped folders. Changes are written to `config.toml` (the previous version is kept as `config.toml.bak`). The live watcher and the viewer pick up new folders at once, and a background sync catalogs them, with no restart.
  - **Pause:** the folder is not scanned or watched; its photos, notes and tags stay in the catalog, as with an unplugged drive.
  - **Protect:** photoApp never deletes, moves or replaces files in that folder, including Delete, bulk move and duplicate clean-up.
  - **Remove:** the folder leaves the library. Nothing on disk is touched; its items keep their notes and come back if you add the folder again.
- **Sort: similar together.** Look-alike pictures are placed next to each other, closest matches side by side, biggest groups first; pictures with no look-alike follow by date. Each tile in a group shows a colored bar and a `≈group · size` badge. A *closeness* picker (near-identical → loosely similar) sets how alike pictures must be. Uses the duplicate fingerprints, so it covers images that have been fingerprinted (after a sync).
- **Library folders** in the sidebar: show only items that have a copy in one folder.
- **Duplicates across all folders** (new Duplicates tab).
  - **Identical copies:** byte-for-byte the same file.
  - **Lower-quality copies:** the same picture resized, re-compressed, converted (PNG / JPEG / WebP) or stripped of camera data. Found with a picture fingerprint plus shape and date checks.
  - **Look-alikes** (same size and format, all with camera data, e.g. burst shots) are listed for review and never removed automatically. They can be marked *Not duplicates*.
  - **Which copy is kept:** the highest-quality one (most pixels, then camera data, then lossless over lossy, then least compressed) or the one in the highest-priority folder. Original names win over `IMG (2).jpg` / `Copy of …`. Each group says why its copy was kept, and any copy can be picked instead.
  - **The other copies** go to the Recycle Bin or a review folder outside the library. Their notes, tags, rating, label and albums are merged into the kept copy.
  - **Undo:** every clean-up can be undone as a whole, from the toast or from *Recent clean-ups*. Undo restores the files and reverts the merged notes and tags.
  - **Compare side by side**, with a full-size view of each copy.
  - **One-click clean-up** of all identical and lower-quality copies.
  - **Automatic clean-up after each sync** (Settings; off by default).
- **Duplicate settings:** what counts as a duplicate (strict / normal / loose matching, same date taken, include videos, anywhere vs. only between folders), which copy to keep, what happens to the others, and automatic clean-up.
### Changed
- Path rows are re-attributed to the right library folder on every sync, so renamed or re-added folders are counted correctly.
- The header's *Duplicates (N)* count includes the same photo at a lower quality.

## [0.4.3] - 2026-10-13
### Added
- **Hide tagged**: one button in the filter bar (or *hide all* next to *Categories & tags* in the sidebar) hides every item that already has a tag. It stays on while you switch views, folders, years, albums or searches, so you can work through what's left to tag. An orange *hiding all tagged* chip shows it's on; click ✕ on the chip or the button again to show tagged items. Choosing a tag or category to show turns it off.

## [0.4.2] - 2026-10-13
### Added
- Right-click menu on photos: **Copy to…**, View, Show in Explorer, Open file, Delete / Restore. Right-clicking a photo outside the selection selects just that photo.
- **Copy to…** (right-click, Inspector, or bulk panel) opens a Windows folder picker and copies the selected files to any folder (USB drive, network share, a client folder). Originals, notes and tags are untouched. Never overwrites: identical files already there are skipped, different files with the same name are saved as `name (2).jpg`. Modified dates are kept. The picker reopens in the last folder used.

## [0.4.1] - 2026-10-13
### Added
- Portable install: the app finds its own folder (no hardcoded paths), creates `config/config.toml` on first run pointing at your Pictures folder, and reads the ffmpeg location from `PHOTOAPP_FFMPEG`, `config.toml` or `PATH`.
- Library folders are granted to the viewer at runtime from `config.toml` (no paths in `tauri.conf.json`).
- `config/config.example.toml`, MIT license, contributing guide, architecture notes, screenshots.
### Fixed
- Dates taken from file names (`2013-06-13_164706.jpg`) were stored as `2013-06-13_` without the time; existing catalogs are repaired once on launch. Invalid times in file names are ignored.
- Tags created by approving AI suggestions now appear in the sidebar immediately.
- "Thinking" models that put their answer in the reasoning field are handled; running out of tokens reports a clear message. Default *Max reply tokens* raised to 4096.

## [0.4.0] - 2026-10-13
### Added
- AI assistant for OpenAI-compatible (LM Studio, Ollama, llama.cpp, vLLM, OpenAI) and Anthropic-compatible APIs: photo review (description, tags, note), approval queue, plain-English *Ask* box, background jobs with progress and cancel.
- Per-photo change history for every edit (user or AI) with undo per change, per photo, or per AI run.
- Sidebar views *AI: waiting approval* and *AI: not reviewed yet*.
### Fixed
- Schema migrations could race on the first launch after an upgrade.

## [0.3.x] - 2026-10-12
### Added
- Delete to the Windows Recycle Bin (verified per drive) with Undo, *Recently deleted* and Restore.
- Live re-sync when whole folders are created, moved or renamed; name + date matching for photos that were both moved and edited.
- Hide / show by tag or category; Photos / Videos toggle; separate photo, video and total counts.
- Mouse-wheel zoom, pan and double-click zoom in the viewer.

## [0.2.x] - 2026-10-12
### Added
- Tags with categories, notes, ratings, favorites, color labels, albums, saved searches, full-text search over all of them.
- Selection, bulk editing, bulk move with preview and undo, CSV export, daily catalog backups, Manage tab, activity log.
### Fixed
- Catalog robustness: edited-in-place files, mixed path separators, offline drives, stale duplicate groups, files edited while the app was closed.

## [0.1.0] - 2026-10-11
- Initial catalog: scan, SHA-256 identity, EXIF dates, thumbnails, virtualized grid, viewer, filters, duplicate finder, rename / move with dry-run and undo.
