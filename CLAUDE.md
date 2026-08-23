# aip-cli

Rust CLI: install and switch Claude / Grok / Pi plugin modes per folder.

## `.aip-removed`

`aip-cli` with no command (or `aip-cli refresh`) lists installed plugins and recopies selected ones from the last folder/URL they were ingested from, then refreshes Claude/Grok. Simpler than `setup <folder>` — no `make prepare`. Sources live in the store's `.aip-sources.toml`.

`aip-cli remove` (names optional — omit to pick several to mark `.aip-removed`; store and, when present, source) writes a gitignored `.aip-removed` in the plugin folder (store copy, and the source folder if run from a plugins repo). `setup` and ingest skip that plugin. Uninstalls from Claude/Codex; does not delete the store copy.

If a directory contains `.aip-removed`, do not install, ingest, audit, evolve, or otherwise mutate that plugin (`steer-plugin-update`, `plugin-auditor`, `plugin-updater`, `plugin-evolver`, and similar). Stop and tell the user. Delete the file only if they explicitly want it reinstalled.

`aip-cli list --plugins` marks these `[removed]`.
