//! Our own abstraction over the Claude Code plugin/marketplace CLI and the
//! Claude-side state that `aip-cli` keeps fresh for its store plugins.
//!
//! Setup must guarantee, for each store plugin, that Claude ends up **installed**
//! and serving the store's **latest code**. Three Claude-side files matter:
//! - `~/.claude/plugins/known_marketplaces.json` — a frozen `installLocation`
//!   per registered marketplace (which can drift to an old source-repo path),
//! - `~/.claude/plugins/plugin-catalog-cache.json` — a component/token catalog, and
//! - `~/.claude/plugins/installed_plugins.json` — which `<plugin>@<marketplace>`
//!   are actually installed (empty right after a user wipes plugins).
//!
//! [`sync_installed_plugin`] does, per plugin: refresh the marketplace
//! (`marketplace update`, or re-register `remove`+`add <store-path>` when
//! `installLocation` has drifted), then — if the plugin is not present in
//! `installed_plugins.json` — `plugin install <plugin>@<marketplace>`. Marketplace
//! registration always precedes install. Keys use the plugin's **manifest** name
//! and the **marketplace** name (from `marketplace.json`), never the store
//! directory name. Every host call flows through a [`CommandRunner`] so the exact
//! command shapes stay unit-testable without touching `claude` or `$HOME`.

use crate::runner::{CommandRunner, Invocation};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// `claude plugin marketplace add <path>` — (re)register a local plugin
/// directory as a marketplace. Run from `cwd`.
pub fn marketplace_add_invocation(path: &Path, cwd: &Path) -> Invocation {
    let p = path.to_string_lossy().into_owned();
    Invocation::new("claude", &["plugin", "marketplace", "add", &p], cwd)
}

/// `claude plugin marketplace remove <name>` — drop a marketplace registration
/// (and, with it, the stale catalog/known-marketplace entries for `name`).
pub fn marketplace_remove_invocation(name: &str, cwd: &Path) -> Invocation {
    Invocation::new("claude", &["plugin", "marketplace", "remove", name], cwd)
}

/// `claude plugin marketplace update <name>` — refresh a marketplace so
/// `plugin-catalog-cache.json` / `known_marketplaces.json` stop serving stale
/// content for `name`.
pub fn marketplace_update_invocation(name: &str, cwd: &Path) -> Invocation {
    Invocation::new("claude", &["plugin", "marketplace", "update", name], cwd)
}

/// `claude plugin install <plugin>@<marketplace>` — install a plugin from a
/// registered marketplace (default scope `user`).
pub fn install_invocation(plugin: &str, marketplace: &str, cwd: &Path) -> Invocation {
    let spec = format!("{plugin}@{marketplace}");
    Invocation::new("claude", &["plugin", "install", &spec], cwd)
}

// ─── marketplace.json (the store plugin's own marketplace manifest) ───────────

#[derive(Deserialize)]
struct MarketplaceManifest {
    #[serde(default)]
    name: Option<String>,
}

/// The marketplace name Claude registers a store plugin under: the `name` in the
/// plugin's `.claude-plugin/marketplace.json`, falling back to `dir_name` when
/// that file is absent, unreadable, or has no non-empty `name`. This is NOT the
/// store directory name — e.g. dir `flutter` registers as `flutter-pivara`.
pub fn resolve_marketplace_name(store_plugin_dir: &Path, dir_name: &str) -> String {
    let path = store_plugin_dir
        .join(".claude-plugin")
        .join("marketplace.json");
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<MarketplaceManifest>(&text)
            .ok()
            .and_then(|m| m.name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| dir_name.to_string()),
        Err(_) => dir_name.to_string(),
    }
}

// ─── known_marketplaces.json ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct MarketplaceEntry {
    #[serde(rename = "installLocation", default)]
    install_location: Option<String>,
}

/// Parse `known_marketplaces.json` into `name -> installLocation`. The file is a
/// flat object mapping each marketplace name to an entry carrying an
/// `installLocation` string; entries without one are skipped. Malformed JSON
/// yields an empty map so callers degrade gracefully.
pub fn parse_known_marketplaces(json: &str) -> BTreeMap<String, String> {
    let parsed: BTreeMap<String, MarketplaceEntry> = serde_json::from_str(json).unwrap_or_default();
    parsed
        .into_iter()
        .filter_map(|(name, entry)| entry.install_location.map(|loc| (name, loc)))
        .collect()
}

/// True when `install_location` points inside `store_root` — i.e. the recorded
/// registration still resolves to the aip store rather than a drifted source
/// path (e.g. an old `~/projects/.../plugins/<name>`). A registration that has
/// drifted must be re-registered so Claude loads the store's latest code.
pub fn install_location_into_store(install_location: &str, store_root: &Path) -> bool {
    Path::new(install_location).starts_with(store_root)
}

/// Read and parse `~/.claude/plugins/known_marketplaces.json`. Absent/unreadable
/// file yields an empty map.
pub fn read_known_marketplaces(home: &Path) -> BTreeMap<String, String> {
    let path = home
        .join(".claude")
        .join("plugins")
        .join("known_marketplaces.json");
    match std::fs::read_to_string(path) {
        Ok(text) => parse_known_marketplaces(&text),
        Err(_) => BTreeMap::new(),
    }
}

// ─── installed_plugins.json (v2) ─────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct InstalledPlugins {
    #[serde(default)]
    plugins: BTreeMap<String, Vec<serde_json::Value>>,
}

/// Parse `installed_plugins.json` (v2) into the set of installed keys. Each key
/// is `<pluginManifestName>@<marketplaceName>` mapping to an array of install
/// records; a key counts as installed only when its array is non-empty. Absent
/// or malformed JSON yields an empty set.
pub fn parse_installed_plugins(json: &str) -> BTreeSet<String> {
    let parsed: InstalledPlugins = serde_json::from_str(json).unwrap_or_default();
    parsed
        .plugins
        .into_iter()
        .filter(|(_, records)| !records.is_empty())
        .map(|(key, _)| key)
        .collect()
}

/// Read `~/.claude/plugins/installed_plugins.json` and return the set of
/// installed `<plugin>@<marketplace>` keys. Absent/unreadable → empty set.
pub fn read_installed_plugins(home: &Path) -> BTreeSet<String> {
    let path = home
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    match std::fs::read_to_string(path) {
        Ok(text) => parse_installed_plugins(&text),
        Err(_) => BTreeSet::new(),
    }
}

/// True when `<plugin>@<marketplace>` is recorded as installed for `home`.
pub fn is_installed(home: &Path, plugin: &str, marketplace: &str) -> bool {
    read_installed_plugins(home).contains(&format!("{plugin}@{marketplace}"))
}

/// Guarantee a store plugin is installed for Claude and serving the store's
/// latest code.
///
/// 1. Marketplace refresh, keyed by `marketplace` (the marketplace.json name):
///    if the recorded `installLocation` has drifted off `store_root`, re-register
///    (`remove` + `add <store_root>/<dir_name>`); otherwise `marketplace update`.
/// 2. Install guarantee: if `<manifest>@<marketplace>` is not present in
///    `installed_plugins.json`, `plugin install <manifest>@<marketplace>`.
///
/// Marketplace registration always precedes install. Best-effort throughout — a
/// plugin the user never registered with Claude (or a missing `claude`) must not
/// abort setup.
pub fn sync_installed_plugin<R: CommandRunner + ?Sized>(
    runner: &R,
    home: &Path,
    store_root: &Path,
    dir_name: &str,
    manifest: &str,
    marketplace: &str,
    cwd: &Path,
) {
    // 1. Marketplace refresh.
    let known = read_known_marketplaces(home);
    let drifted = match known.get(marketplace) {
        Some(loc) => !install_location_into_store(loc, store_root),
        // Not yet registered: `update` is a harmless no-op; the initial `add`
        // is the plugin's own `make setup` job.
        None => false,
    };
    if drifted {
        let _ = runner.run(&marketplace_remove_invocation(marketplace, cwd));
        let _ = runner.run(&marketplace_add_invocation(&store_root.join(dir_name), cwd));
    } else {
        let _ = runner.run(&marketplace_update_invocation(marketplace, cwd));
    }

    // 2. Install guarantee — after the marketplace is registered/fresh.
    if !is_installed(home, manifest, marketplace) {
        let _ = runner.run(&install_invocation(manifest, marketplace, cwd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingRunner;
    use tempfile::TempDir;

    /// Write a known_marketplaces.json under `home` with one entry.
    fn write_known(home: &Path, name: &str, install_location: &str) {
        let plugins = home.join(".claude").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(
            plugins.join("known_marketplaces.json"),
            format!(r#"{{"{name}":{{"installLocation":"{install_location}"}}}}"#),
        )
        .unwrap();
    }

    /// Write an installed_plugins.json (v2) under `home` marking `keys` installed.
    fn write_installed(home: &Path, keys: &[&str]) {
        let plugins = home.join(".claude").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let entries: Vec<String> = keys
            .iter()
            .map(|k| format!(r#""{k}":[{{"scope":"user","installPath":"/p","version":"1.0.0"}}]"#))
            .collect();
        std::fs::write(
            plugins.join("installed_plugins.json"),
            format!(r#"{{"version":2,"plugins":{{{}}}}}"#, entries.join(",")),
        )
        .unwrap();
    }

    #[test]
    fn invocation_shapes() {
        assert_eq!(
            marketplace_add_invocation(Path::new("/s/p"), Path::new("/cwd")).display(),
            "claude plugin marketplace add /s/p"
        );
        assert_eq!(
            marketplace_remove_invocation("foo", Path::new("/cwd")).display(),
            "claude plugin marketplace remove foo"
        );
        assert_eq!(
            marketplace_update_invocation("foo", Path::new("/cwd")).display(),
            "claude plugin marketplace update foo"
        );
        assert_eq!(
            install_invocation("flutter-pivara", "flutter-pivara", Path::new("/cwd")).display(),
            "claude plugin install flutter-pivara@flutter-pivara"
        );
    }

    #[test]
    fn resolve_marketplace_name_reads_manifest_then_falls_back() {
        let tmp = TempDir::new().unwrap();
        // Store dir `flutter` whose marketplace.json names it `flutter-pivara`.
        let dir = tmp.path().join("flutter");
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("marketplace.json"),
            r#"{"name":"flutter-pivara","plugins":[]}"#,
        )
        .unwrap();
        assert_eq!(resolve_marketplace_name(&dir, "flutter"), "flutter-pivara");

        // No marketplace.json → fall back to the directory name.
        let bare = tmp.path().join("software-engineer");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(
            resolve_marketplace_name(&bare, "software-engineer"),
            "software-engineer"
        );

        // Present but empty/missing name → fall back to the directory name.
        let empty = tmp.path().join("plug");
        std::fs::create_dir_all(empty.join(".claude-plugin")).unwrap();
        std::fs::write(
            empty.join(".claude-plugin").join("marketplace.json"),
            r#"{"name":"","plugins":[]}"#,
        )
        .unwrap();
        assert_eq!(resolve_marketplace_name(&empty, "plug"), "plug");
    }

    #[test]
    fn parse_installed_plugins_reads_v2_shape() {
        // Real v2 shape: name != dir (flutter-pivara), a multi-entry array, and
        // an empty array that must NOT count as installed.
        let json = r#"{
            "version": 2,
            "plugins": {
                "software-engineer@software-engineer": [
                    {"scope":"user","installPath":"/Users/x/.claude/plugins/cache/software-engineer","version":"1.2.0","installedAt":"t","lastUpdated":"t","gitCommitSha":"abc"}
                ],
                "flutter-pivara@flutter-pivara": [
                    {"scope":"user","installPath":"/p1","version":"1.0.0"},
                    {"scope":"project","installPath":"/p2","version":"1.0.0"}
                ],
                "never-installed@never-installed": []
            }
        }"#;
        let set = parse_installed_plugins(json);
        assert!(set.contains("software-engineer@software-engineer"));
        assert!(set.contains("flutter-pivara@flutter-pivara"));
        // Empty array → not installed.
        assert!(!set.contains("never-installed@never-installed"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn parse_installed_plugins_absent_or_malformed_is_empty() {
        assert!(parse_installed_plugins(r#"{"version":2,"plugins":{}}"#).is_empty());
        assert!(parse_installed_plugins("{not json").is_empty());
        assert!(parse_installed_plugins("").is_empty());
    }

    #[test]
    fn parse_known_marketplaces_reads_install_locations() {
        // Real shape observed on disk: flat name -> { source, installLocation }.
        let json = r#"{
            "frontend": {
                "source": {"source":"directory","path":"/Users/x/.aip-cli/plugins/frontend"},
                "installLocation": "/Users/x/.aip-cli/plugins/frontend",
                "lastUpdated": "2026-07-02T21:41:55.251Z"
            },
            "product": {
                "source": {"source":"directory","path":"/Users/x/projects/claude/plugins/product"},
                "installLocation": "/Users/x/projects/claude/plugins/product"
            },
            "no-location": {"source": {}}
        }"#;
        let m = parse_known_marketplaces(json);
        assert_eq!(
            m.get("frontend").map(String::as_str),
            Some("/Users/x/.aip-cli/plugins/frontend")
        );
        assert_eq!(
            m.get("product").map(String::as_str),
            Some("/Users/x/projects/claude/plugins/product")
        );
        // Entry lacking installLocation is dropped, not defaulted.
        assert!(!m.contains_key("no-location"));
    }

    #[test]
    fn parse_known_marketplaces_malformed_is_empty() {
        assert!(parse_known_marketplaces("{not json").is_empty());
        assert!(parse_known_marketplaces("").is_empty());
    }

    #[test]
    fn install_location_into_store_detects_drift() {
        let store = Path::new("/Users/x/.aip-cli/plugins");
        assert!(install_location_into_store(
            "/Users/x/.aip-cli/plugins/frontend",
            store
        ));
        // Drifted to an old source-repo path → not in store.
        assert!(!install_location_into_store(
            "/Users/x/projects/claude/plugins/product",
            store
        ));
    }

    #[test]
    fn sync_updates_then_installs_when_missing() {
        // Not drifted (no known file) → `update`; not installed → `install`.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            "frontend",
            Path::new("/cwd"),
        );
        assert_eq!(
            runner.lines(),
            vec![
                "claude plugin marketplace update frontend".to_string(),
                "claude plugin install frontend@frontend".to_string(),
            ]
        );
    }

    #[test]
    fn sync_skips_install_when_already_installed() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        write_installed(&home, &["frontend@frontend"]);
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            "frontend",
            Path::new("/cwd"),
        );
        // Marketplace refresh only; NO install command.
        assert_eq!(
            runner.lines(),
            vec!["claude plugin marketplace update frontend"]
        );
    }

    #[test]
    fn sync_reregisters_on_drift_then_installs_with_manifest_and_marketplace_names() {
        // Store dir `flutter`, manifest `flutter-pivara`, marketplace `flutter-pivara`.
        // installLocation drifted → remove+add(store dir path); not installed → install.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        write_known(&home, "flutter-pivara", "/old/source/plugins/flutter");
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "flutter",
            "flutter-pivara",
            "flutter-pivara",
            Path::new("/cwd"),
        );
        let add_path = store.join("flutter");
        assert_eq!(
            runner.lines(),
            vec![
                "claude plugin marketplace remove flutter-pivara".to_string(),
                format!(
                    "claude plugin marketplace add {}",
                    add_path.to_string_lossy()
                ),
                "claude plugin install flutter-pivara@flutter-pivara".to_string(),
            ]
        );
    }

    #[test]
    fn sync_updates_only_when_installed_and_location_in_store() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let loc = store.join("frontend");
        write_known(&home, "frontend", &loc.to_string_lossy());
        write_installed(&home, &["frontend@frontend"]);
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            "frontend",
            Path::new("/cwd"),
        );
        assert_eq!(
            runner.lines(),
            vec!["claude plugin marketplace update frontend"]
        );
    }
}
