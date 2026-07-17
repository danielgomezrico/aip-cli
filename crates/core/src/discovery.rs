//! Discovery of installable plugins on disk.
//!
//! A directory is a plugin when it contains both a `.claude-plugin/plugin.json`
//! manifest and a `Makefile`. Discovery mirrors the behaviour of the original
//! `run_all.py`: scan `plugins/` (falling back to the repo root), sorted by
//! directory name, skipping anything missing a manifest or Makefile.

use crate::manifest::PluginManifest;
use crate::store::read_subdirs;
use std::path::{Path, PathBuf};

/// A discovered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plugin {
    /// Directory name on disk, e.g. `android`.
    pub dir_name: String,
    /// Manifest `name`, e.g. `android-native`.
    pub name: String,
    pub version: String,
    /// Whether the plugin's Makefile defines a `prepare:` target.
    pub has_prepare: bool,
    pub path: PathBuf,
}

/// The directory plugins are scanned from: `<repo>/plugins` if it exists,
/// otherwise `<repo>` itself (backwards compatibility).
pub fn plugins_root(repo_root: &Path) -> PathBuf {
    let nested = repo_root.join("plugins");
    if nested.is_dir() {
        nested
    } else {
        repo_root.to_path_buf()
    }
}

/// Discover all plugins under `plugins_root`, sorted by directory name.
pub fn discover_plugins(plugins_root: &Path) -> std::io::Result<Vec<Plugin>> {
    let entries: Vec<PathBuf> = read_subdirs(plugins_root)?;

    let mut plugins = Vec::new();
    for dir in entries {
        let manifest_path = dir.join(".claude-plugin").join("plugin.json");
        let makefile = dir.join("Makefile");
        if !manifest_path.exists() || !makefile.exists() {
            continue;
        }
        let manifest = match PluginManifest::read(&dir) {
            Ok(m) => m,
            Err(_) => continue, // malformed manifest: skip, matching run_all.py
        };
        let dir_name = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let has_prepare = makefile_has_prepare(&makefile);
        plugins.push(Plugin {
            dir_name,
            name: manifest.name,
            version: manifest.version,
            has_prepare,
            path: dir,
        });
    }
    Ok(plugins)
}

/// True when the Makefile declares a `prepare:` target at the start of a line.
fn makefile_has_prepare(makefile: &Path) -> bool {
    match std::fs::read_to_string(makefile) {
        Ok(text) => text.lines().any(|l| l.starts_with("prepare:")),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_plugin(root: &Path, dir: &str, name: &str, version: &str, prepare: bool) {
        let p = root.join(dir);
        fs::create_dir_all(p.join(".claude-plugin")).unwrap();
        fs::write(
            p.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
        let mk = if prepare {
            "prepare:\n\techo hi\nsetup:\n\techo s\n"
        } else {
            "setup:\n\techo s\n"
        };
        fs::write(p.join("Makefile"), mk).unwrap();
    }

    #[test]
    fn discovers_sorted_plugins() {
        let tmp = TempDir::new().unwrap();
        make_plugin(tmp.path(), "zeta", "zeta-plug", "1.2.0", false);
        make_plugin(tmp.path(), "alpha", "alpha-plug", "0.1.0", true);
        let found = discover_plugins(tmp.path()).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].dir_name, "alpha");
        assert_eq!(found[0].name, "alpha-plug");
        assert!(found[0].has_prepare);
        assert_eq!(found[1].dir_name, "zeta");
        assert!(!found[1].has_prepare);
    }

    #[test]
    fn skips_dirs_without_manifest_or_makefile() {
        let tmp = TempDir::new().unwrap();
        make_plugin(tmp.path(), "good", "good", "1.0.0", false);
        fs::create_dir_all(tmp.path().join("no-manifest")).unwrap();
        fs::write(tmp.path().join("no-manifest").join("Makefile"), "x:\n").unwrap();
        let found = discover_plugins(tmp.path()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "good");
    }

    #[test]
    fn skips_malformed_manifest() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("bad");
        fs::create_dir_all(p.join(".claude-plugin")).unwrap();
        fs::write(p.join(".claude-plugin").join("plugin.json"), "{not json").unwrap();
        fs::write(p.join("Makefile"), "setup:\n").unwrap();
        assert!(discover_plugins(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn plugins_root_prefers_nested_plugins_dir() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("plugins")).unwrap();
        assert_eq!(plugins_root(tmp.path()), tmp.path().join("plugins"));
    }

    #[test]
    fn plugins_root_falls_back_to_repo_root() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(plugins_root(tmp.path()), tmp.path());
    }
}
