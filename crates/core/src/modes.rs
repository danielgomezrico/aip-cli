//! Plugin-name resolution against the store universe.
//!
//! Selectors are plugin directory names (or 1-based indices into that list,
//! or a unique manifest `.name` alias). Users pick plugins independently —
//! there is no Role×Domain catalog or generated mode set.

use crate::manifest::PluginManifest;
use crate::removed::is_removed;
use crate::store::{plugins_dir, read_plugin_dirs};
use thiserror::Error;

/// Every plugin in the store that is not marked `.aip-removed`, as directory
/// names in [`read_plugin_dirs`] order (byte-lex).
pub fn all_plugins() -> Vec<String> {
    read_plugin_dirs(&plugins_dir())
        .into_iter()
        .filter(|d| !is_removed(d))
        .filter_map(|d| d.file_name().and_then(|s| s.to_str()).map(str::to_string))
        .collect()
}

/// Numbered picker lines matching [`all_plugins`] order (`"1) apple"`, …).
pub fn numbered_plugin_lines() -> Vec<String> {
    all_plugins()
        .into_iter()
        .enumerate()
        .map(|(i, name)| format!("{}) {}", i + 1, name))
        .collect()
}

/// Errors from resolving a plugin selector string.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModeError {
    #[error("unknown mode: {0}")]
    Unknown(String),
    #[error("no mode selected")]
    Empty,
}

/// Resolve a single selector token (a dir name, unique manifest alias, or
/// 1-based index) to a store directory name.
fn resolve_token(token: &str, plugins: &[String]) -> Result<String, ModeError> {
    if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) {
        let idx: usize = token
            .parse()
            .map_err(|_| ModeError::Unknown(token.to_string()))?;
        return plugins
            .get(idx.wrapping_sub(1))
            .filter(|_| idx >= 1)
            .cloned()
            .ok_or_else(|| ModeError::Unknown(token.to_string()));
    }
    if plugins.iter().any(|p| p == token) {
        return Ok(token.to_string());
    }
    let aliases: Vec<&String> = plugins
        .iter()
        .filter(|dir_name| {
            PluginManifest::read(&plugins_dir().join(dir_name))
                .ok()
                .is_some_and(|m| m.name == token)
        })
        .collect();
    match aliases.as_slice() {
        [one] => Ok((*one).clone()),
        _ => Err(ModeError::Unknown(token.to_string())),
    }
}

/// The resolved result of a selection: the chosen plugin dir names and the
/// same names in store order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Chosen plugin directory names, in selection order (deduped).
    pub chosen: Vec<String>,
    /// Those names in [`all_plugins`] order.
    pub enabled: Vec<String>,
}

impl Resolution {
    /// Returns `(enabled, disabled)` partition over the store universe.
    pub fn partition(&self) -> (Vec<String>, Vec<String>) {
        let mut on = Vec::new();
        let mut off = Vec::new();
        for p in all_plugins() {
            if self.enabled.contains(&p) {
                on.push(p);
            } else {
                off.push(p);
            }
        }
        (on, off)
    }
}

/// Resolve a raw selector string (e.g. `"apple, 3"` or `"apple backend-go"`)
/// into chosen plugin dir names and the enabled set in store order.
pub fn resolve(selector: &str) -> Result<Resolution, ModeError> {
    let tokens: Vec<&str> = selector
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err(ModeError::Empty);
    }

    let plugins = all_plugins();
    let mut chosen: Vec<String> = Vec::new();
    for tok in tokens {
        let name = resolve_token(tok, &plugins)?;
        if !chosen.iter().any(|c| c == &name) {
            chosen.push(name);
        }
    }

    let enabled: Vec<String> = plugins.into_iter().filter(|p| chosen.contains(p)).collect();

    Ok(Resolution { chosen, enabled })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::removed::REMOVED_MARKER;
    use crate::store::with_store_dir;
    use std::fs;
    use tempfile::TempDir;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn write_plugin(root: &std::path::Path, dir: &str, name: &str, version: &str) {
        let meta = root.join(dir).join(".claude-plugin");
        fs::create_dir_all(&meta).unwrap();
        fs::write(
            meta.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
    }

    fn ac1_store() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        for (dir, name) in [
            ("apple", "apple"),
            ("backend-python", "backend-python"),
            ("investigation", "investigation"),
            ("backend-go", "backend-go"),
            ("frontend", "frontend"),
        ] {
            write_plugin(&plugins, dir, name, "1.0.0");
        }
        tmp
    }

    #[test]
    fn all_plugins_includes_uncategorized_store_dirs() {
        let tmp = ac1_store();
        fs::create_dir_all(tmp.path().join("plugins").join("not-a-plugin")).unwrap();
        with_store_dir(tmp.path(), || {
            assert_eq!(
                all_plugins(),
                names(&[
                    "apple",
                    "backend-go",
                    "backend-python",
                    "frontend",
                    "investigation"
                ])
            );
        });
    }

    #[test]
    fn all_plugins_excludes_aip_removed() {
        let tmp = ac1_store();
        let gone = tmp.path().join("plugins");
        write_plugin(&gone, "gone", "gone", "1.0.0");
        fs::write(gone.join("gone").join(REMOVED_MARKER), "").unwrap();
        with_store_dir(tmp.path(), || {
            let listed = all_plugins();
            assert!(!listed.contains(&"gone".to_string()));
            assert!(listed.contains(&"apple".to_string()));
        });
    }

    #[test]
    fn all_plugins_empty_store_is_empty() {
        let tmp = TempDir::new().unwrap();
        with_store_dir(tmp.path(), || {
            assert_eq!(all_plugins(), Vec::<String>::new());
        });
    }

    #[test]
    fn numbered_plugin_lines_match_all_plugins_order() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            let lines = numbered_plugin_lines();
            assert_eq!(lines[0], "1) apple");
            let plugins = all_plugins();
            assert_eq!(lines.len(), plugins.len());
            for (i, name) in plugins.iter().enumerate() {
                assert_eq!(lines[i], format!("{}) {}", i + 1, name));
            }
        });
    }

    #[test]
    fn resolve_by_name() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            let r = resolve("apple").unwrap();
            assert_eq!(r.chosen, names(&["apple"]));
            assert_eq!(r.enabled, names(&["apple"]));
        });
    }

    #[test]
    fn resolve_by_index() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            assert_eq!(resolve("1").unwrap().chosen, names(&["apple"]));
            assert_eq!(resolve("1").unwrap().enabled, names(&["apple"]));
        });
    }

    #[test]
    fn resolve_frontend_is_only_that_plugin() {
        let tmp = TempDir::new().unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        for dir in ["apple", "frontend", "software-engineer"] {
            write_plugin(&plugins, dir, dir, "1.0.0");
        }
        with_store_dir(tmp.path(), || {
            let r = resolve("frontend").unwrap();
            assert_eq!(r.chosen, names(&["frontend"]));
            assert_eq!(r.enabled, names(&["frontend"]));
        });
    }

    #[test]
    fn resolve_unknown_name_errors() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            assert_eq!(
                resolve("nope").unwrap_err(),
                ModeError::Unknown("nope".into())
            );
            assert_eq!(
                resolve("mobile").unwrap_err(),
                ModeError::Unknown("mobile".into())
            );
        });
    }

    #[test]
    fn resolve_out_of_range_index_errors() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            assert_eq!(
                resolve("999").unwrap_err(),
                ModeError::Unknown("999".into())
            );
            assert_eq!(resolve("0").unwrap_err(), ModeError::Unknown("0".into()));
        });
    }

    #[test]
    fn resolve_empty_errors() {
        assert_eq!(resolve("   ").unwrap_err(), ModeError::Empty);
    }

    #[test]
    fn resolve_comma_and_index_mix() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            let space = resolve("apple backend-go").unwrap();
            assert_eq!(space.chosen, names(&["apple", "backend-go"]));
            assert_eq!(space.enabled, names(&["apple", "backend-go"]));
            let comma = resolve("apple, backend-go").unwrap();
            assert_eq!(comma.chosen, names(&["apple", "backend-go"]));
            assert_eq!(comma.enabled, names(&["apple", "backend-go"]));
            let mixed = resolve("apple, 2").unwrap();
            assert_eq!(mixed.chosen, names(&["apple", "backend-go"]));
            assert_eq!(mixed.enabled, names(&["apple", "backend-go"]));
        });
    }

    #[test]
    fn resolve_dedups_repeated_selection() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            let r = resolve("apple apple 1").unwrap();
            assert_eq!(r.chosen, names(&["apple"]));
            assert_eq!(r.enabled, names(&["apple"]));
        });
    }

    #[test]
    fn resolve_unique_manifest_alias() {
        let tmp = TempDir::new().unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        write_plugin(&plugins, "android", "android-native", "1.0.0");
        with_store_dir(tmp.path(), || {
            let r = resolve("android-native").unwrap();
            assert_eq!(r.chosen, names(&["android"]));
            assert_eq!(r.enabled, names(&["android"]));
        });
    }

    #[test]
    fn resolve_ambiguous_manifest_errors() {
        let tmp = TempDir::new().unwrap();
        let plugins = tmp.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        write_plugin(&plugins, "one", "shared", "1.0.0");
        write_plugin(&plugins, "two", "shared", "1.0.0");
        with_store_dir(tmp.path(), || {
            assert_eq!(
                resolve("shared").unwrap_err(),
                ModeError::Unknown("shared".into())
            );
        });
    }

    #[test]
    fn partition_covers_all_plugins() {
        let tmp = ac1_store();
        with_store_dir(tmp.path(), || {
            let r = resolve("apple").unwrap();
            let (on, off) = r.partition();
            assert_eq!(on, names(&["apple"]));
            let universe = all_plugins();
            assert_eq!(on.len() + off.len(), universe.len());
            for p in &universe {
                assert!(on.contains(p) ^ off.contains(p));
            }
        });
    }
}
