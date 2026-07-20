//! Reading each AI agent's *current* plugin state from its on-disk config.
//!
//! [`crate::mode_apply`] writes state via host CLIs; the `doctor` command needs
//! to read it back. Agents store this differently:
//!
//! * Claude Code — `~/.claude/settings.json`, key `enabledPlugins`: a map of
//!   `"<name>@<marketplace>" -> bool` (the plugin name is the part before `@`).
//! * Grok — `~/.grok/config.toml`, table `[plugins]` with `enabled` and
//!   `disabled` string arrays.
//! * Pi — `~/.pi/agent/settings.json`, key `packages`: array of local paths
//!   (or npm/git sources). Aip-cli installs store plugins as local package
//!   paths; the last path component is the plugin name.
//!
//! Parsing is pure and unit-tested; the thin [`read_state`] wrapper does the
//! filesystem read and is the only part that touches disk.

use crate::mode_apply::Target;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The plugins one agent knows about, partitioned by whether they're enabled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentPlugins {
    /// Enabled plugin names, sorted and deduplicated.
    pub enabled: Vec<String>,
    /// Known-but-disabled plugin names, sorted and deduplicated.
    pub disabled: Vec<String>,
}

impl AgentPlugins {
    /// Build from raw lists, normalising to sorted+deduped order so output and
    /// equality are stable regardless of the source file's ordering.
    fn normalized(mut enabled: Vec<String>, mut disabled: Vec<String>) -> Self {
        enabled.sort();
        enabled.dedup();
        disabled.sort();
        disabled.dedup();
        Self { enabled, disabled }
    }

    /// Every plugin the agent knows about (enabled ∪ disabled), sorted.
    pub fn installed(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .enabled
            .iter()
            .chain(self.disabled.iter())
            .cloned()
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// True when `plugin` is currently enabled.
    pub fn is_enabled(&self, plugin: &str) -> bool {
        self.enabled.iter().any(|p| p == plugin)
    }
}

// ─── Claude Code: ~/.claude/settings.json ────────────────────────────────────

#[derive(Deserialize, Default)]
struct ClaudeSettings {
    #[serde(rename = "enabledPlugins", default)]
    enabled_plugins: BTreeMap<String, bool>,
}

/// Parse Claude's `enabledPlugins` map. Keys look like `"product@product"`; the
/// plugin name is the segment before `@`. `true` => enabled, `false` =>
/// disabled. Malformed JSON yields an empty result rather than erroring, so
/// `doctor` degrades gracefully on a corrupt settings file.
pub fn parse_claude(settings_json: &str) -> AgentPlugins {
    let parsed: ClaudeSettings = serde_json::from_str(settings_json).unwrap_or_default();
    let mut enabled = Vec::new();
    let mut disabled = Vec::new();
    for (key, on) in parsed.enabled_plugins {
        let name = key.split('@').next().unwrap_or(&key).to_string();
        if on {
            enabled.push(name);
        } else {
            disabled.push(name);
        }
    }
    // The same plugin can appear under several marketplace keys; enabled in any
    // one wins, so a name is "disabled" only when it is never enabled.
    disabled.retain(|d| !enabled.contains(d));
    AgentPlugins::normalized(enabled, disabled)
}

// ─── Grok: ~/.grok/config.toml ───────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct GrokConfig {
    #[serde(default)]
    plugins: GrokPlugins,
}

#[derive(Deserialize, Default)]
struct GrokPlugins {
    #[serde(default)]
    enabled: Vec<String>,
    #[serde(default)]
    disabled: Vec<String>,
}

/// Parse Grok's `[plugins]` table (`enabled` / `disabled` arrays). Malformed
/// TOML yields an empty result rather than erroring.
pub fn parse_grok(config_toml: &str) -> AgentPlugins {
    let parsed: GrokConfig = toml::from_str(config_toml).unwrap_or_default();
    AgentPlugins::normalized(parsed.plugins.enabled, parsed.plugins.disabled)
}

// ─── Pi: ~/.pi/agent/settings.json ───────────────────────────────────────────

/// Parse Pi's `packages` array into enabled plugin names. Entries may be strings
/// (`"../../.aip-cli/plugins/product"`) or objects (`{"source":"..."}`). The
/// plugin name is the last path segment of a local path (or the raw source for
/// npm/git). Malformed JSON yields empty.
pub fn parse_pi(settings_json: &str) -> AgentPlugins {
    let Ok(v) = serde_json::from_str::<Value>(settings_json) else {
        return AgentPlugins::default();
    };
    let Some(packages) = v.get("packages").and_then(|p| p.as_array()) else {
        return AgentPlugins::default();
    };
    let mut enabled = Vec::new();
    for entry in packages {
        let source = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(m) => m
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or(""),
            _ => "",
        };
        if source.is_empty() {
            continue;
        }
        if let Some(name) = pi_package_plugin_name(source) {
            enabled.push(name);
        }
    }
    AgentPlugins::normalized(enabled, Vec::new())
}

/// Best-effort plugin name from a pi package source string.
fn pi_package_plugin_name(source: &str) -> Option<String> {
    let s = source.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    // Local path (absolute, relative, or ./...) — last component is the name.
    // npm:/git: sources keep the package id after the scheme.
    let name = if let Some(rest) = s.strip_prefix("npm:") {
        rest.rsplit('/').next().unwrap_or(rest)
    } else if let Some(rest) = s.strip_prefix("git:") {
        rest.rsplit('/').next().unwrap_or(rest)
    } else {
        Path::new(s)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(s)
    };
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name.to_string())
    }
}

// ─── filesystem ──────────────────────────────────────────────────────────────

/// The config file that holds `target`'s plugin state, relative to `home`.
pub fn state_path(target: Target, home: &Path) -> PathBuf {
    match target {
        Target::ClaudeCode => home.join(".claude").join("settings.json"),
        Target::Grok => home.join(".grok").join("config.toml"),
        Target::Pi => home.join(".pi").join("agent").join("settings.json"),
    }
}

/// Read and parse `target`'s plugin state from disk. Returns `None` when the
/// config file is absent or unreadable — i.e. the agent isn't configured.
pub fn read_state(target: Target, home: &Path) -> Option<AgentPlugins> {
    let text = std::fs::read_to_string(state_path(target, home)).ok()?;
    Some(match target {
        Target::ClaudeCode => parse_claude(&text),
        Target::Grok => parse_grok(&text),
        Target::Pi => parse_pi(&text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_splits_marketplace_suffix_and_partitions() {
        let json = r#"{
            "enabledPlugins": {
                "product@product": true,
                "ai-architecture@ai-architecture": true,
                "frontend@frontend": false
            },
            "other": 1
        }"#;
        let p = parse_claude(json);
        assert_eq!(p.enabled, vec!["ai-architecture", "product"]);
        assert_eq!(p.disabled, vec!["frontend"]);
        assert!(p.is_enabled("product"));
        assert!(!p.is_enabled("frontend"));
    }

    #[test]
    fn parse_claude_missing_key_is_empty() {
        let p = parse_claude(r#"{"theme":"dark"}"#);
        assert!(p.enabled.is_empty());
        assert!(p.disabled.is_empty());
    }

    #[test]
    fn parse_claude_malformed_is_empty_not_panic() {
        assert_eq!(parse_claude("{not json"), AgentPlugins::default());
    }

    #[test]
    fn parse_claude_enabled_wins_over_disabled_for_same_plugin() {
        // The same plugin installed from two marketplaces, one on and one off.
        // It must not show up as both enabled and disabled: being enabled
        // anywhere means enabled, so it must be absent from `disabled`.
        let json = r#"{"enabledPlugins":{"x@a":true,"x@b":false}}"#;
        let p = parse_claude(json);
        assert_eq!(p.enabled, vec!["x"]);
        assert!(p.disabled.is_empty(), "x must not be listed as disabled");
        assert!(p.is_enabled("x"));
    }

    #[test]
    fn parse_claude_key_without_suffix() {
        let p = parse_claude(r#"{"enabledPlugins":{"solo":true}}"#);
        assert_eq!(p.enabled, vec!["solo"]);
    }

    #[test]
    fn parse_grok_reads_enabled_and_disabled_arrays() {
        let toml = r#"
[cli]
installer = "internal"

[plugins]
enabled = ["ai-architecture", "dev-ops"]
disabled = ["software-engineer", "product"]
"#;
        let p = parse_grok(toml);
        assert_eq!(p.enabled, vec!["ai-architecture", "dev-ops"]);
        assert_eq!(p.disabled, vec!["product", "software-engineer"]);
        assert_eq!(
            p.installed(),
            vec!["ai-architecture", "dev-ops", "product", "software-engineer"]
        );
    }

    #[test]
    fn parse_grok_missing_table_is_empty() {
        let p = parse_grok("[cli]\ninstaller = \"internal\"\n");
        assert_eq!(p, AgentPlugins::default());
    }

    #[test]
    fn parse_grok_malformed_is_empty_not_panic() {
        assert_eq!(parse_grok("= = ="), AgentPlugins::default());
    }

    #[test]
    fn parse_pi_local_paths_and_object_form() {
        let json = r#"{
            "defaultModel": "x",
            "packages": [
                "../../.aip-cli/plugins/product",
                {"source": "/Users/x/.aip-cli/plugins/software-engineer", "skills": []},
                "npm:@foo/bar",
                ""
            ]
        }"#;
        let p = parse_pi(json);
        assert_eq!(
            p.enabled,
            vec!["bar", "product", "software-engineer"]
        );
        assert!(p.disabled.is_empty());
    }

    #[test]
    fn parse_pi_missing_or_malformed_is_empty() {
        assert_eq!(parse_pi("{}"), AgentPlugins::default());
        assert_eq!(parse_pi("{not json"), AgentPlugins::default());
        assert_eq!(
            parse_pi(r#"{"packages":[]}"#),
            AgentPlugins::default()
        );
    }

    #[test]
    fn state_path_per_target() {
        let home = Path::new("/home/x");
        assert_eq!(
            state_path(Target::ClaudeCode, home),
            Path::new("/home/x/.claude/settings.json")
        );
        assert_eq!(
            state_path(Target::Grok, home),
            Path::new("/home/x/.grok/config.toml")
        );
        assert_eq!(
            state_path(Target::Pi, home),
            Path::new("/home/x/.pi/agent/settings.json")
        );
    }

    #[test]
    fn read_state_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(read_state(Target::ClaudeCode, tmp.path()).is_none());
        assert!(read_state(Target::Grok, tmp.path()).is_none());
        assert!(read_state(Target::Pi, tmp.path()).is_none());
    }

    #[test]
    fn read_state_reads_claude_from_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude").join("settings.json"),
            r#"{"enabledPlugins":{"product@product":true}}"#,
        )
        .unwrap();
        let p = read_state(Target::ClaudeCode, tmp.path()).unwrap();
        assert_eq!(p.enabled, vec!["product"]);
    }

    #[test]
    fn installed_dedups_overlap() {
        let p =
            AgentPlugins::normalized(vec!["a".into(), "b".into()], vec!["b".into(), "c".into()]);
        assert_eq!(p.installed(), vec!["a", "b", "c"]);
    }
}
