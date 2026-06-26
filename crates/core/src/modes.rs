//! Curated plugin "modes" and selector resolution.
//!
//! A *mode* maps a short key (e.g. `mobile`) to the set of plugins that should be
//! enabled for it. Users select modes by name or by 1-based index, comma- or
//! space-separated — a faithful port of the original Makefile `mode` target.
//!
//! ## Plugin Categorization (Faceted Model)
//!
//! Plugins are categorized via two orthogonal facets (following Nielsen Norman design):
//! - **role**: What the user does (architect, engineer, product-manager, career, hobby)
//! - **domain**: What they work on (general, web, mobile, backend, infra)
//!
//! Each plugin belongs to exactly one role and one domain. This prevents synonym
//! fragmentation and makes mode composition deterministic. See PLUGIN_METADATA below.

use thiserror::Error;

/// Plugin role: what the user does with these tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Architect,
    Engineer,
    ProductManager,
    Career,
    Hobby,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Architect => "architect",
            Role::Engineer => "engineer",
            Role::ProductManager => "product-manager",
            Role::Career => "career",
            Role::Hobby => "hobby",
        }
    }
}

/// Plugin domain: what problem area it targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    General,
    Web,
    Mobile,
    Backend,
    Infra,
}

impl Domain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Domain::General => "general",
            Domain::Web => "web",
            Domain::Mobile => "mobile",
            Domain::Backend => "backend",
            Domain::Infra => "infra",
        }
    }
}

/// Plugin categorization: role + domain pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginMetadata {
    pub role: Role,
    pub domain: Domain,
}

/// Plugin registry with canonical metadata.
/// When adding a new plugin:
/// 1. Add it to this const list with its role + domain
/// 2. Update modes that should include it (see registry() below)
/// 3. No free-form strings — use the Role/Domain enums above
pub const PLUGIN_METADATA: &[(&str, PluginMetadata)] = &[
    ("ai-architecture", PluginMetadata { role: Role::Architect, domain: Domain::General }),
    ("software-engineer", PluginMetadata { role: Role::Engineer, domain: Domain::General }),
    ("flutter", PluginMetadata { role: Role::Engineer, domain: Domain::Mobile }),
    ("frontend", PluginMetadata { role: Role::Engineer, domain: Domain::Web }),
    ("frontend-native", PluginMetadata { role: Role::Engineer, domain: Domain::Web }),
    ("python-ai", PluginMetadata { role: Role::Engineer, domain: Domain::Backend }),
    ("dev-ops", PluginMetadata { role: Role::Engineer, domain: Domain::Infra }),
    ("product", PluginMetadata { role: Role::ProductManager, domain: Domain::General }),
    ("job-hunter", PluginMetadata { role: Role::Career, domain: Domain::General }),
    ("music-librarian", PluginMetadata { role: Role::Hobby, domain: Domain::General }),
    ("home-assistant", PluginMetadata { role: Role::Hobby, domain: Domain::General }),
    ("oz", PluginMetadata { role: Role::Hobby, domain: Domain::General }),
    ("real-estate-hunter", PluginMetadata { role: Role::Career, domain: Domain::General }),
    ("stock-advisor", PluginMetadata { role: Role::Career, domain: Domain::General }),
];

/// Every plugin the repo knows about, derived from PLUGIN_METADATA (canonical order).
pub fn all_plugins() -> Vec<&'static str> {
    PLUGIN_METADATA.iter().map(|(name, _)| *name).collect()
}

/// Look up a plugin's metadata by name.
pub fn get_plugin_metadata(name: &str) -> Option<PluginMetadata> {
    PLUGIN_METADATA.iter()
        .find(|(plugin_name, _)| *plugin_name == name)
        .map(|(_, meta)| *meta)
}

/// A named, curated subset of plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub key: &'static str,
    pub plugins: Vec<&'static str>,
}

/// The ordered mode registry. Index in this list (1-based) is the picker number.
/// Helper: expand plugins by facet query. Filter PLUGIN_METADATA by role and/or domain.
/// Returns plugins matching ALL specified criteria (intersection).
///
/// # Examples
/// - `plugins_by_facets(&[Role::Engineer], &[Domain::Web])` → frontend, frontend-native, software-engineer, ai-architecture
/// - `plugins_by_facets(&[Role::Engineer], &[])` → all engineers (software-engineer, flutter, frontend, etc.)
fn plugins_by_facets(roles: &[Role], domains: &[Domain]) -> Vec<&'static str> {
    PLUGIN_METADATA.iter()
        .filter(|(_, meta)| {
            (roles.is_empty() || roles.contains(&meta.role)) &&
            (domains.is_empty() || domains.contains(&meta.domain))
        })
        .map(|(name, _)| *name)
        .collect()
}

pub fn registry() -> Vec<Mode> {
    fn m(key: &'static str, plugins: Vec<&'static str>) -> Mode {
        Mode { key, plugins }
    }

    vec![
        // Curated modes: hand-selected plugins for specific workflows
        m("mobile", vec!["ai-architecture", "software-engineer", "flutter"]),
        m("frontend", vec!["ai-architecture", "software-engineer", "frontend", "frontend-native"]),
        m("backend", vec!["ai-architecture", "software-engineer", "python-ai"]),
        m("infra", vec!["ai-architecture", "software-engineer", "dev-ops"]),
        m("full-stack", vec!["ai-architecture", "software-engineer", "frontend", "frontend-native", "python-ai", "dev-ops"]),

        m("architect", vec!["ai-architecture"]),
        m("engineer", vec!["ai-architecture", "software-engineer"]),
        m("startup", vec!["ai-architecture", "software-engineer", "product"]),
        m("product", vec!["ai-architecture", "software-engineer", "product"]),

        m("career", vec!["job-hunter", "real-estate-hunter", "stock-advisor"]),
        m("hobby", vec!["music-librarian", "oz", "home-assistant"]),

        // Niche modes
        m("jobs", vec!["ai-architecture", "job-hunter"]),
        m("music", vec!["ai-architecture", "music-librarian"]),
        m("marketing", vec!["ai-architecture", "product"]),
        m("home-devops", vec!["ai-architecture", "home-assistant", "dev-ops"]),
        m("oz", vec!["ai-architecture", "oz"]),
        m("realestate", vec!["ai-architecture", "real-estate-hunter"]),
        m("investments-stock", vec!["ai-architecture", "stock-advisor"]),
        m("investments-all", vec!["ai-architecture", "stock-advisor", "real-estate-hunter"]),

        // Meta modes
        m("all", all_plugins()),
        m("minimal", vec!["ai-architecture", "software-engineer"]),
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
    let enabled: Vec<&'static str> = all_plugins()
        .into_iter()
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
        assert_eq!(by_key("all").unwrap().plugins, all_plugins());
    }

    #[test]
    fn every_mode_references_known_plugins() {
        let known = all_plugins();
        for mode in registry() {
            for p in mode.plugins {
                assert!(
                    known.contains(&p),
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
        // Mobile mode: architect + engineer + flutter (canonical PLUGIN_METADATA order)
        assert_eq!(r.enabled, vec!["ai-architecture", "software-engineer", "flutter"]);
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
        // Union of frontend + product in canonical order
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
        let r = resolve("mobile, jobs").unwrap(); // mobile + jobs
        assert_eq!(r.chosen, vec!["mobile", "jobs"]);
        assert!(r.enabled.contains(&"job-hunter"));
        assert!(r.enabled.contains(&"flutter"));
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
        assert_eq!(on.len() + off.len(), all_plugins().len());
    }
}
