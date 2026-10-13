# Changelog

All notable changes to photoApp. Versions follow [Semantic Versioning](https://semver.org).

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
