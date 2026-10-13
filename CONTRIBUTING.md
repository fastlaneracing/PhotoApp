# Contributing to photoApp

Thanks for your interest! Bug reports, ideas and pull requests are welcome.

## Reporting a bug
Open an issue with:
- what you did, what you expected, and what happened;
- your Windows version and photoApp version (see `CHANGELOG.md`);
- for build problems, the end of `build.log`;
- for AI problems, the server (LM Studio, Ollama…) and model name.

Please don't attach your `catalog.db` or personal photos.

## Building
See *Requirements* and *Development* in the [README](README.md). In short:

```bat
build.bat                      :: release exe
cd src-tauri && cargo test     :: Rust tests
```

## Pull requests
- Keep changes focused; one feature or fix per PR.
- `cargo build` should finish **without warnings**, and `cargo test` should pass.
- Never weaken the safety rules (see `docs/ARCHITECTURE.md`): no permanent deletes, no overwriting files, AI changes only after user approval, every catalog edit recorded in the change history.
- Schema changes must be additive and idempotent (see `db.rs` → `migrate`), because existing catalogs are upgraded in place.
- Describe how you tested it.

## Code layout
`src-tauri/src` holds the Rust back end (one module per concern), `frontend/src` the Svelte UI. `docs/ARCHITECTURE.md` explains the data model and the main flows.
