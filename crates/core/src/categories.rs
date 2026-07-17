//! Runtime plugin categorization overlay, persisted as a TOML sidecar.
//!
//! Built-in plugins are categorized at compile time in
//! [`crate::modes::PLUGIN_METADATA`]. Ingested plugins can't be — the const is
//! fixed — so their role/domain is captured at ingest time and stored here, in
//! `<config_dir>/categories.toml`:
//!
//! ```toml
//! [plugins."my-plugin"]
//! role = "engineer"
//! domain = "web"
//! ```
//!
//! At startup the CLI loads this overlay and hands it to
//! [`crate::modes::set_overlay`], so a categorized ingested plugin flows through
//! `all_plugins()`, the facet-generated modes, and `doctor` exactly like a
//! built-in. A missing or malformed sidecar yields an empty overlay rather than
//! an error, so a corrupt file can never brick the CLI.

use crate::modes::{Domain, PluginMetadata, Role};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Sidecar file name inside the config dir.
pub const CATEGORIES_FILE: &str = "categories.toml";

/// Path to the categories sidecar inside `config_dir`.
pub fn categories_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CATEGORIES_FILE)
}

#[derive(Deserialize)]
struct SidecarFile {
    #[serde(default)]
    plugins: BTreeMap<String, RawEntry>,
}

#[derive(Deserialize)]
struct RawEntry {
    role: String,
    domain: String,
}

/// Load all categorized plugins from the sidecar in `config_dir`. A missing or
/// malformed file yields an empty overlay (never an error). Entries whose role
/// or domain don't parse are skipped individually.
pub fn load(config_dir: &Path) -> Vec<(String, PluginMetadata)> {
    match std::fs::read_to_string(categories_path(config_dir)) {
        Ok(text) => parse(&text),
        Err(_) => Vec::new(),
    }
}

fn parse(text: &str) -> Vec<(String, PluginMetadata)> {
    let file: SidecarFile = match toml::from_str(text) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    file.plugins
        .into_iter()
        .filter_map(|(name, e)| {
            let role = Role::parse(&e.role)?;
            let domain = Domain::parse(&e.domain)?;
            Some((name, PluginMetadata { role, domain }))
        })
        .collect()
}

/// True when `name` already has a category — either a built-in const entry or a
/// sidecar overlay entry under `config_dir`.
pub fn is_categorized(config_dir: &Path, name: &str) -> bool {
    crate::modes::builtin_metadata(name).is_some()
        || load(config_dir).iter().any(|(n, _)| n == name)
}

/// Persist (or update) one plugin's category in the sidecar under `config_dir`,
/// preserving every other entry already present. Creates the dir/file as needed.
/// The file is rewritten sorted by name for a stable, diff-friendly result.
pub fn save(config_dir: &Path, name: &str, meta: PluginMetadata) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let mut entries: BTreeMap<String, PluginMetadata> = load(config_dir).into_iter().collect();
    entries.insert(name.to_string(), meta);

    let mut text = String::from("# Plugin categories captured at ingest time by aip-cli.\n");
    for (n, m) in &entries {
        text.push_str(&format!(
            "\n[plugins.\"{}\"]\nrole = \"{}\"\ndomain = \"{}\"\n",
            n,
            m.role.as_str(),
            m.domain.as_str()
        ));
    }
    std::fs::write(categories_path(config_dir), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_missing_file_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let meta = PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Web,
        };
        save(tmp.path(), "my-plugin", meta).unwrap();
        let loaded = load(tmp.path());
        assert_eq!(loaded, vec![("my-plugin".to_string(), meta)]);
        assert!(is_categorized(tmp.path(), "my-plugin"));
    }

    #[test]
    fn save_preserves_existing_entries() {
        let tmp = TempDir::new().unwrap();
        save(
            tmp.path(),
            "a-plugin",
            PluginMetadata {
                role: Role::Engineer,
                domain: Domain::Backend,
            },
        )
        .unwrap();
        save(
            tmp.path(),
            "b-plugin",
            PluginMetadata {
                role: Role::Hobby,
                domain: Domain::General,
            },
        )
        .unwrap();
        let names: Vec<String> = load(tmp.path()).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["a-plugin", "b-plugin"]);
    }

    #[test]
    fn save_updates_in_place() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        save(
            dir,
            "p",
            PluginMetadata {
                role: Role::Engineer,
                domain: Domain::Web,
            },
        )
        .unwrap();
        let updated = PluginMetadata {
            role: Role::Architect,
            domain: Domain::General,
        };
        save(dir, "p", updated).unwrap();
        assert_eq!(load(dir), vec![("p".to_string(), updated)]);
    }

    #[test]
    fn malformed_file_yields_empty() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(categories_path(tmp.path()), "{ not toml").unwrap();
        assert!(load(tmp.path()).is_empty());
    }

    #[test]
    fn unknown_role_or_domain_is_skipped() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            categories_path(tmp.path()),
            "[plugins.\"x\"]\nrole = \"wizard\"\ndomain = \"web\"\n\
             [plugins.\"y\"]\nrole = \"engineer\"\ndomain = \"web\"\n",
        )
        .unwrap();
        let names: Vec<String> = load(tmp.path()).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["y"]);
    }
}
