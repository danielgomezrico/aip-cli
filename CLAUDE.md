# aip-cli

Rust CLI: install and switch Claude / Grok / Pi plugin modes per folder.

## `.aip-removed`

`aip-cli remove` writes a gitignored `.aip-removed` in the plugin folder (store copy, and the source folder if run from a plugins repo). `setup` and ingest skip that plugin.

If a directory contains `.aip-removed`, do not install, ingest, audit, evolve, or otherwise mutate that plugin (`steer-plugin-update`, `plugin-auditor`, `plugin-updater`, `plugin-evolver`, and similar). Stop and tell the user. Delete the file only if they explicitly want it reinstalled.

`aip-cli list --plugins` marks these `[removed]`.
