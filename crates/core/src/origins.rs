//! Last-known ingest sources for store plugins.
//!
//! After `setup <folder-or-url>` we remember where each plugin came from so a
//! later refresh can recopy from that path without the user passing it again.

use crate::config::find_repo_root;
use crate::store::is_plugin_dir;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Sidecar inside the store plugins directory.
pub const SOURCES_FILE: &str = ".aip-sources.toml";

/// Path of the origins sidecar under `store_plugins`.
pub fn sources_path(store_plugins: &Path) -> PathBuf {
    store_plugins.join(SOURCES_FILE)
}

/// Recorded ingest sources: a last folder/URL plus a per-plugin map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Origins {
    #[serde(default)]
    pub last: Option<String>,
    #[serde(default)]
    pub plugins: BTreeMap<String, String>,
}

/// Load origins from `store_plugins`. Missing or malformed → empty.
pub fn load(store_plugins: &Path) -> Origins {
    match std::fs::read_to_string(sources_path(store_plugins)) {
        Ok(text) => parse(&text),
        Err(_) => Origins::default(),
    }
}

fn parse(text: &str) -> Origins {
    toml::from_str(text).unwrap_or_default()
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Rewrite the sidecar. Creates `store_plugins` if needed.
pub fn save(store_plugins: &Path, origins: &Origins) -> std::io::Result<()> {
    std::fs::create_dir_all(store_plugins)?;
    let mut text = String::from("# Last ingest sources recorded by aip-cli.\n");
    if let Some(last) = &origins.last {
        text.push_str(&format!("last = \"{}\"\n", toml_escape(last)));
    }
    if !origins.plugins.is_empty() {
        text.push_str("\n[plugins]\n");
        for (name, source) in &origins.plugins {
            text.push_str(&format!(
                "\"{}\" = \"{}\"\n",
                toml_escape(name),
                toml_escape(source)
            ));
        }
    }
    std::fs::write(sources_path(store_plugins), text)
}

/// Merge `last` and/or plugin entries into the sidecar.
pub fn record_many(
    store_plugins: &Path,
    last: Option<&str>,
    plugins: impl IntoIterator<Item = (String, String)>,
) -> std::io::Result<()> {
    let mut o = load(store_plugins);
    if let Some(last) = last {
        o.last = Some(last.to_string());
    }
    for (name, source) in plugins {
        o.plugins.insert(name, source);
    }
    save(store_plugins, &o)
}

/// Remember one plugin's source path or URL.
pub fn record_plugin(store_plugins: &Path, name: &str, source: &str) -> std::io::Result<()> {
    record_many(
        store_plugins,
        None,
        [(name.to_string(), source.to_string())],
    )
}

/// Remember the last ingest folder or URL.
pub fn record_last(store_plugins: &Path, last: &str) -> std::io::Result<()> {
    record_many(store_plugins, Some(last), [])
}

/// Where to recopy `dir_name` from: recorded source, then `last/<dir_name>`
/// if that is a plugin dir, then a matching dir in a plugins repo above `cwd`.
pub fn resolve_origin(dir_name: &str, store_plugins: &Path, cwd: &Path) -> Option<String> {
    let o = load(store_plugins);
    if let Some(s) = o.plugins.get(dir_name) {
        if !s.is_empty() {
            return Some(s.clone());
        }
    }
    if let Some(last) = o.last.as_deref() {
        let candidate = Path::new(last).join(dir_name);
        if is_plugin_dir(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    if let Some(repo) = find_repo_root(cwd) {
        let candidate = crate::discovery::plugins_root(&repo).join(dir_name);
        if is_plugin_dir(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_plugin(dir: &Path) {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            r#"{"name":"x","version":"1.0.0"}"#,
        )
        .unwrap();
    }

    #[test]
    fn load_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(load(tmp.path()), Origins::default());
    }

    #[test]
    fn malformed_is_empty() {
        let tmp = TempDir::new().unwrap();
        fs::write(sources_path(tmp.path()), "{ not toml").unwrap();
        assert_eq!(load(tmp.path()), Origins::default());
    }

    #[test]
    fn record_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("plugins");
        record_many(
            &store,
            Some("/src"),
            [("frontend".into(), "/src/frontend".into())],
        )
        .unwrap();
        let o = load(&store);
        assert_eq!(o.last.as_deref(), Some("/src"));
        assert_eq!(
            o.plugins.get("frontend").map(String::as_str),
            Some("/src/frontend")
        );
    }

    #[test]
    fn record_plugin_preserves_last_and_others() {
        let tmp = TempDir::new().unwrap();
        record_last(tmp.path(), "/src").unwrap();
        record_plugin(tmp.path(), "a", "/src/a").unwrap();
        record_plugin(tmp.path(), "b", "/src/b").unwrap();
        record_plugin(tmp.path(), "a", "/other/a").unwrap();
        let o = load(tmp.path());
        assert_eq!(o.last.as_deref(), Some("/src"));
        assert_eq!(o.plugins.get("a").map(String::as_str), Some("/other/a"));
        assert_eq!(o.plugins.get("b").map(String::as_str), Some("/src/b"));
    }

    #[test]
    fn resolve_prefers_recorded_plugin() {
        let tmp = TempDir::new().unwrap();
        record_plugin(tmp.path(), "frontend", "/remembered/frontend").unwrap();
        assert_eq!(
            resolve_origin("frontend", tmp.path(), tmp.path()).as_deref(),
            Some("/remembered/frontend")
        );
    }

    #[test]
    fn resolve_falls_back_to_last_child() {
        let tmp = TempDir::new().unwrap();
        let last = tmp.path().join("incoming");
        write_plugin(&last.join("frontend"));
        record_last(tmp.path(), &last.to_string_lossy()).unwrap();
        let got = resolve_origin("frontend", tmp.path(), tmp.path()).unwrap();
        assert!(got.ends_with("incoming/frontend"), "{got}");
    }

    #[test]
    fn resolve_falls_back_to_cwd_plugins_repo() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        write_plugin(&repo.join("plugins").join("frontend"));
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let got = resolve_origin("frontend", &store, &repo).unwrap();
        assert!(got.ends_with("plugins/frontend"), "{got}");
    }

    #[test]
    fn resolve_empty_when_nothing_matches() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_origin("missing", tmp.path(), tmp.path()).is_none());
    }

    #[test]
    fn empty_recorded_source_is_ignored() {
        let tmp = TempDir::new().unwrap();
        record_plugin(tmp.path(), "frontend", "").unwrap();
        assert!(resolve_origin("frontend", tmp.path(), tmp.path()).is_none());
    }
}
