//! Curated plugin "modes" and selector resolution.
//!
//! A *mode* maps a short key (e.g. `mobile`) to the set of plugins that should be
//! enabled for it. Users select modes by name or by 1-based index, comma- or
//! space-separated — a faithful port of the original Makefile `mode` target.

use thiserror::Error;

/// Every plugin the repo knows about, in canonical order.
pub const ALL_PLUGINS: &[&str] = &[
    "ai-architecture",
    "software-engineer",
    "flutter-pivara",
    "android-native",
    "frontend",
    "frontend-native",
    "product",
    "job-hunter",
    "music-librarian",
    "python-ai",
    "home-assistant",
    "dev-ops",
    "oz",
    "real-estate-hunter",
    "stock-advisor",
];

/// A named, curated subset of plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub key: &'static str,
    pub plugins: Vec<&'static str>,
}

/// The ordered mode registry. Index in this list (1-based) is the picker number.
pub fn registry() -> Vec<Mode> {
    fn m(key: &'static str, plugins: &[&'static str]) -> Mode {
        Mode {
            key,
            plugins: plugins.to_vec(),
        }
    }
    vec![
        m(
            "mobile",
            &[
                "ai-architecture",
                "software-engineer",
                "flutter-pivara",
                "android-native",
                "product",
            ],
        ),
        m(
            "frontend",
            &[
                "ai-architecture",
                "software-engineer",
                "frontend",
                "frontend-native",
                "product",
            ],
        ),
        m(
            "product",
            &["ai-architecture", "software-engineer", "product"],
        ),
        m("jobs", &["ai-architecture", "job-hunter"]),
        m("music", &["ai-architecture", "music-librarian"]),
        m(
            "python",
            &[
                "ai-architecture",
                "software-engineer",
                "python-ai",
                "product",
            ],
        ),
        m("marketing", &["ai-architecture", "product"]),
        m(
            "home-devops",
            &["ai-architecture", "home-assistant", "dev-ops"],
        ),
        m("oz", &["ai-architecture", "oz"]),
        m("realestate", &["ai-architecture", "real-estate-hunter"]),
        m("investments-stock", &["ai-architecture", "stock-advisor"]),
        m(
            "investments-all",
            &["ai-architecture", "stock-advisor", "real-estate-hunter"],
        ),
        m("all", ALL_PLUGINS),
        m("minimal", &["ai-architecture", "software-engineer"]),
    ]
}

/// Errors from resolving a mode selector string.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModeError {
    #[error("unknown mode: {0}")]
    Unknown(String),
    #[error("no mode selected")]
    Empty,
}

/// Look up a mode by its key.
pub fn by_key(key: &str) -> Option<Mode> {
    registry().into_iter().find(|m| m.key == key)
}

/// Resolve a single selector token (a name or a 1-based index) to a mode key.
fn resolve_token(token: &str) -> Result<&'static str, ModeError> {
    let reg = registry();
    // All-digits => index into the registry (1-based), matching the Makefile.
    if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) {
        let idx: usize = token
            .parse()
            .map_err(|_| ModeError::Unknown(token.to_string()))?;
        return reg
            .get(idx.wrapping_sub(1))
            .filter(|_| idx >= 1)
            .map(|m| m.key)
            .ok_or_else(|| ModeError::Unknown(token.to_string()));
    }
    reg.into_iter()
        .find(|m| m.key == token)
        .map(|m| m.key)
        .ok_or_else(|| ModeError::Unknown(token.to_string()))
}

/// The resolved result of a selection: the chosen mode keys and the union of
/// plugins that should be enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub chosen: Vec<&'static str>,
    /// Union of plugins to enable, in `ALL_PLUGINS` canonical order.
    pub enabled: Vec<&'static str>,
}

impl Resolution {
    /// Returns `(enabled, disabled)` partition over `ALL_PLUGINS`.
    pub fn partition(&self) -> (Vec<&'static str>, Vec<&'static str>) {
        let mut on = Vec::new();
        let mut off = Vec::new();
        for &p in ALL_PLUGINS {
            if self.enabled.contains(&p) {
                on.push(p);
            } else {
                off.push(p);
            }
        }
        (on, off)
    }
}

/// Resolve a raw selector string (e.g. `"mobile, 3"` or `"frontend product"`)
/// into the chosen modes and the union of enabled plugins.
pub fn resolve(selector: &str) -> Result<Resolution, ModeError> {
    let tokens: Vec<&str> = selector
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err(ModeError::Empty);
    }

    let mut chosen: Vec<&'static str> = Vec::new();
    for tok in tokens {
        let key = resolve_token(tok)?;
        if !chosen.contains(&key) {
            chosen.push(key);
        }
    }

    // Union of plugins, expressed in canonical ALL_PLUGINS order for stable output.
    let mut union: Vec<&'static str> = Vec::new();
    for key in &chosen {
        if let Some(mode) = by_key(key) {
            for p in mode.plugins {
                if !union.contains(&p) {
                    union.push(p);
                }
            }
        }
    }
    let enabled: Vec<&'static str> = ALL_PLUGINS
        .iter()
        .copied()
        .filter(|p| union.contains(p))
        .collect();

    Ok(Resolution { chosen, enabled })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_expected_modes() {
        let keys: Vec<_> = registry().iter().map(|m| m.key).collect();
        assert_eq!(keys[0], "mobile");
        assert_eq!(keys.last().copied(), Some("minimal"));
        assert!(keys.contains(&"investments-all"));
    }

    #[test]
    fn all_mode_equals_all_plugins() {
        assert_eq!(by_key("all").unwrap().plugins, ALL_PLUGINS.to_vec());
    }

    #[test]
    fn every_mode_references_known_plugins() {
        for mode in registry() {
            for p in mode.plugins {
                assert!(
                    ALL_PLUGINS.contains(&p),
                    "{} -> unknown plugin {}",
                    mode.key,
                    p
                );
            }
        }
    }

    #[test]
    fn resolve_by_name() {
        let r = resolve("mobile").unwrap();
        assert_eq!(r.chosen, vec!["mobile"]);
        assert_eq!(
            r.enabled,
            vec![
                "ai-architecture",
                "software-engineer",
                "flutter-pivara",
                "android-native",
                "product"
            ]
        );
    }

    #[test]
    fn resolve_by_index() {
        // 1 == mobile
        assert_eq!(resolve("1").unwrap().chosen, vec!["mobile"]);
    }

    #[test]
    fn resolve_multiple_unions_in_canonical_order() {
        let r = resolve("frontend product").unwrap();
        assert_eq!(r.chosen, vec!["frontend", "product"]);
        // product adds no new plugins beyond frontend's set; order stays canonical.
        assert_eq!(
            r.enabled,
            vec![
                "ai-architecture",
                "software-engineer",
                "frontend",
                "frontend-native",
                "product"
            ]
        );
    }

    #[test]
    fn resolve_comma_and_index_mix() {
        let r = resolve("mobile, 4").unwrap(); // 4 == jobs
        assert_eq!(r.chosen, vec!["mobile", "jobs"]);
        assert!(r.enabled.contains(&"job-hunter"));
        assert!(r.enabled.contains(&"flutter-pivara"));
    }

    #[test]
    fn resolve_dedups_repeated_selection() {
        let r = resolve("mobile mobile 1").unwrap();
        assert_eq!(r.chosen, vec!["mobile"]);
    }

    #[test]
    fn resolve_unknown_name_errors() {
        assert_eq!(
            resolve("nope").unwrap_err(),
            ModeError::Unknown("nope".into())
        );
    }

    #[test]
    fn resolve_out_of_range_index_errors() {
        assert_eq!(
            resolve("999").unwrap_err(),
            ModeError::Unknown("999".into())
        );
        assert_eq!(resolve("0").unwrap_err(), ModeError::Unknown("0".into()));
    }

    #[test]
    fn resolve_empty_errors() {
        assert_eq!(resolve("   ").unwrap_err(), ModeError::Empty);
    }

    #[test]
    fn partition_covers_all_plugins() {
        let r = resolve("minimal").unwrap();
        let (on, off) = r.partition();
        assert_eq!(on, vec!["ai-architecture", "software-engineer"]);
        assert_eq!(on.len() + off.len(), ALL_PLUGINS.len());
    }
}
