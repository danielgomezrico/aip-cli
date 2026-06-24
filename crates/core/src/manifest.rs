//! Parsing of a plugin's `.claude-plugin/plugin.json` manifest.

use serde::Deserialize;
use std::path::Path;

/// The subset of `plugin.json` fields the CLI cares about.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

impl PluginManifest {
    /// Parse a manifest from raw JSON text.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Read and parse `<plugin>/.claude-plugin/plugin.json`.
    pub fn read(plugin_dir: &Path) -> std::io::Result<Self> {
        let path = plugin_dir.join(".claude-plugin").join("plugin.json");
        let text = std::fs::read_to_string(path)?;
        Self::from_json(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_version() {
        let m = PluginManifest::from_json(r#"{"name":"product","version":"2.3.0"}"#).unwrap();
        assert_eq!(m.name, "product");
        assert_eq!(m.version, "2.3.0");
    }

    #[test]
    fn version_defaults_when_absent() {
        let m = PluginManifest::from_json(r#"{"name":"oz"}"#).unwrap();
        assert_eq!(m.version, "1.0.0");
    }

    #[test]
    fn ignores_unknown_fields() {
        let m = PluginManifest::from_json(r#"{"name":"x","keywords":["a"],"foo":1}"#).unwrap();
        assert_eq!(m.name, "x");
    }

    #[test]
    fn rejects_missing_name() {
        assert!(PluginManifest::from_json(r#"{"version":"1.0.0"}"#).is_err());
    }
}
