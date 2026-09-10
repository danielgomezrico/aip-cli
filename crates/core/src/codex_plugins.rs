//! Codex CLI plugin marketplace + install. Remove stays a single argv slot.

use crate::claude_plugins::resolve_marketplace_name;
use crate::host_models::{self, Host};
use crate::runner::{CommandRunner, Invocation};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `codex plugin remove <spec>` — uninstall one plugin. `spec` is a single
/// argv slot (do not split on `@`).
pub fn remove_invocation(spec: &str, cwd: &Path) -> Invocation {
    Invocation::new("codex", &["plugin", "remove", spec], cwd)
}

/// `codex plugin marketplace add <path>` — register a local marketplace root.
pub fn marketplace_add_invocation(path: &Path, cwd: &Path) -> Invocation {
    let p = path.to_string_lossy().into_owned();
    Invocation::new("codex", &["plugin", "marketplace", "add", &p], cwd)
}

/// `codex plugin marketplace remove <name>`.
pub fn marketplace_remove_invocation(name: &str, cwd: &Path) -> Invocation {
    Invocation::new("codex", &["plugin", "marketplace", "remove", name], cwd)
}

/// `codex plugin add <plugin>@<marketplace>`.
pub fn add_invocation(plugin: &str, marketplace: &str, cwd: &Path) -> Invocation {
    let spec = format!("{plugin}@{marketplace}");
    Invocation::new("codex", &["plugin", "add", &spec], cwd)
}

/// Marketplace name from `.codex-plugin/marketplace.json`, then Claude's
/// marketplace.json, then `dir_name`.
pub fn resolve_codex_marketplace_name(store_plugin_dir: &Path, dir_name: &str) -> String {
    let path = store_plugin_dir
        .join(".codex-plugin")
        .join("marketplace.json");
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some(name) = serde_json::from_str::<MarketplaceName>(&text)
            .ok()
            .and_then(|m| m.name)
            .filter(|s| !s.is_empty())
        {
            return name;
        }
    }
    resolve_marketplace_name(store_plugin_dir, dir_name)
}

#[derive(Deserialize)]
struct MarketplaceName {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct CodexConfig {
    #[serde(default)]
    marketplaces: BTreeMap<String, MarketplaceSource>,
}

#[derive(Deserialize, Default)]
struct MarketplaceSource {
    #[serde(default)]
    source: Option<String>,
}

fn read_codex_config(home: &Path) -> CodexConfig {
    let path = home.join(".codex").join("config.toml");
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => CodexConfig::default(),
    }
}

fn marketplace_source(home: &Path, marketplace: &str) -> Option<String> {
    read_codex_config(home)
        .marketplaces
        .get(marketplace)
        .and_then(|e| e.source.clone())
}

pub fn source_into_store(source: &str, store_root: &Path, dir_name: &str) -> bool {
    Path::new(source).starts_with(host_models::host_stage_dir(
        store_root,
        Host::Codex,
        dir_name,
    ))
}

pub fn sync_installed_plugin<R: CommandRunner + ?Sized>(
    runner: &R,
    home: &Path,
    store_root: &Path,
    dir_name: &str,
    manifest: &str,
    src: &Path,
    cwd: &Path,
) {
    let marketplace = resolve_codex_marketplace_name(src, dir_name);
    let install_root = host_models::stage_for_host(store_root, src, dir_name, Host::Codex)
        .unwrap_or_else(|| src.to_path_buf());

    let known = marketplace_source(home, &marketplace);
    let needs_add = match known.as_deref() {
        Some(loc) => !source_into_store(loc, store_root, dir_name),
        None => true,
    };
    if needs_add {
        if known.is_some() {
            let _ = runner.run(&marketplace_remove_invocation(&marketplace, cwd));
        }
        let _ = runner.run(&marketplace_add_invocation(&install_root, cwd));
    }

    let _ = runner.run(&add_invocation(manifest, &marketplace, cwd));
}

/// Used by tests that need a known stage path.
pub fn staged_plugin_path(store_root: &Path, dir_name: &str) -> PathBuf {
    host_models::host_stage_dir(store_root, Host::Codex, dir_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingRunner;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_config(home: &Path, body: &str) {
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    fn write_plugin(dir: &Path, marketplace: &str, manifest: &str) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("marketplace.json"),
            format!(
                r#"{{"name":"{marketplace}","plugins":[{{"name":"{manifest}","source":"./"}}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{manifest}","version":"1.0.0"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(
            dir.join("agents").join("lead.md"),
            "---\nmodel: opus\n---\n# Lead\n",
        )
        .unwrap();
    }

    #[test]
    fn invocation_shapes() {
        let inv = remove_invocation("sample@debug", Path::new("/cwd"));
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.args, ["plugin", "remove", "sample@debug"]);
        assert_eq!(inv.display(), "codex plugin remove sample@debug");
        assert_eq!(
            marketplace_add_invocation(Path::new("/s/p"), Path::new("/cwd")).display(),
            "codex plugin marketplace add /s/p"
        );
        assert_eq!(
            add_invocation("frontend", "frontend", Path::new("/cwd")).display(),
            "codex plugin add frontend@frontend"
        );
    }

    #[test]
    fn sync_adds_marketplace_and_plugin_when_missing() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("frontend");
        write_plugin(&src, "frontend", "frontend");
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            &src,
            Path::new("/cwd"),
        );
        let staged = staged_plugin_path(&store, "frontend");
        assert_eq!(
            runner.lines(),
            vec![
                format!("codex plugin marketplace add {}", staged.display()),
                "codex plugin add frontend@frontend".to_string(),
            ]
        );
        let staged_agent = std::fs::read_to_string(staged.join("agents").join("lead.md")).unwrap();
        assert!(staged_agent.contains("model: gpt-5.6"));
        let src_agent = std::fs::read_to_string(src.join("agents").join("lead.md")).unwrap();
        assert!(src_agent.contains("model: opus"));
    }

    #[test]
    fn sync_reregisters_on_drift_then_adds() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("flutter");
        write_plugin(&src, "flutter-pivara", "flutter-pivara");
        write_config(
            &home,
            r#"
[marketplaces.flutter-pivara]
source = "/old/source/plugins/flutter"
"#,
        );
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "flutter",
            "flutter-pivara",
            &src,
            Path::new("/cwd"),
        );
        let staged = staged_plugin_path(&store, "flutter");
        assert_eq!(
            runner.lines(),
            vec![
                "codex plugin marketplace remove flutter-pivara".to_string(),
                format!("codex plugin marketplace add {}", staged.display()),
                "codex plugin add flutter-pivara@flutter-pivara".to_string(),
            ]
        );
    }

    #[test]
    fn sync_readds_an_installed_plugin_so_stage_changes_reach_the_codex_cache() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("frontend");
        write_plugin(&src, "frontend", "frontend");
        let staged = staged_plugin_path(&store, "frontend");
        write_config(
            &home,
            &format!(
                r#"
[marketplaces.frontend]
source = "{}"

[plugins."frontend@frontend"]
enabled = true
"#,
                staged.display()
            ),
        );
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            &src,
            Path::new("/cwd"),
        );
        assert_eq!(
            runner.lines(),
            vec!["codex plugin add frontend@frontend".to_string()]
        );
    }

    #[test]
    fn resolve_prefers_codex_marketplace_json() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("plug");
        std::fs::create_dir_all(dir.join(".codex-plugin")).unwrap();
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".codex-plugin").join("marketplace.json"),
            r#"{"name":"codex-mkt"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("marketplace.json"),
            r#"{"name":"claude-mkt"}"#,
        )
        .unwrap();
        assert_eq!(resolve_codex_marketplace_name(&dir, "plug"), "codex-mkt");
    }
}
