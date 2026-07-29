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
//! fragmentation and makes mode composition deterministic.
//!
//! Categorization comes from two sources, merged into a single *catalog*:
//! - built-in plugins, hardcoded in [`PLUGIN_METADATA`], and
//! - ingested plugins, categorized at ingest time and persisted by
//!   [`crate::categories`], then loaded over the const via [`set_overlay`].
//!
//! Modes are *generated from the catalog by facet* (see [`registry`]) rather than
//! hand-maintained, so they never duplicate or drift, and a newly categorized
//! ingested plugin automatically appears in the modes its facets imply.

use crate::arg_enum::ArgEnum;
use std::sync::OnceLock;
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

    /// Every role, in canonical order — the menu offered when categorizing.
    pub fn all() -> [Role; 5] {
        [
            Role::Architect,
            Role::Engineer,
            Role::ProductManager,
            Role::Career,
            Role::Hobby,
        ]
    }

    /// Parse a role from its string form (accepts a few friendly aliases).
    pub fn parse(s: &str) -> Option<Role> {
        <Role as ArgEnum>::parse(s)
    }
}

impl ArgEnum for Role {
    fn from_normalized(token: &str) -> Option<Role> {
        match token {
            "architect" => Some(Role::Architect),
            "engineer" => Some(Role::Engineer),
            "product-manager" | "product" | "pm" => Some(Role::ProductManager),
            "career" => Some(Role::Career),
            "hobby" => Some(Role::Hobby),
            _ => None,
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

    /// Every domain, in canonical order — the menu offered when categorizing.
    pub fn all() -> [Domain; 5] {
        [
            Domain::General,
            Domain::Web,
            Domain::Mobile,
            Domain::Backend,
            Domain::Infra,
        ]
    }

    /// Parse a domain from its string form (accepts a few friendly aliases).
    pub fn parse(s: &str) -> Option<Domain> {
        <Domain as ArgEnum>::parse(s)
    }
}

impl ArgEnum for Domain {
    fn from_normalized(token: &str) -> Option<Domain> {
        match token {
            "general" | "any" => Some(Domain::General),
            "web" | "frontend" => Some(Domain::Web),
            "mobile" => Some(Domain::Mobile),
            "backend" => Some(Domain::Backend),
            "infra" | "infrastructure" | "devops" => Some(Domain::Infra),
            _ => None,
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
    (
        "ai-architecture",
        PluginMetadata {
            role: Role::Architect,
            domain: Domain::General,
        },
    ),
    (
        "software-engineer",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::General,
        },
    ),
    (
        "flutter",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Mobile,
        },
    ),
    (
        "android",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Mobile,
        },
    ),
    (
        "frontend",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Web,
        },
    ),
    (
        "frontend-native",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Web,
        },
    ),
    (
        "python-ai",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Backend,
        },
    ),
    (
        "dev-ops",
        PluginMetadata {
            role: Role::Engineer,
            domain: Domain::Infra,
        },
    ),
    (
        "product",
        PluginMetadata {
            role: Role::ProductManager,
            domain: Domain::General,
        },
    ),
    (
        "job-hunter",
        PluginMetadata {
            role: Role::Career,
            domain: Domain::General,
        },
    ),
    (
        "music-librarian",
        PluginMetadata {
            role: Role::Hobby,
            domain: Domain::General,
        },
    ),
    (
        "home-assistant",
        PluginMetadata {
            role: Role::Hobby,
            domain: Domain::General,
        },
    ),
    (
        "oz",
        PluginMetadata {
            role: Role::Hobby,
            domain: Domain::General,
        },
    ),
    (
        "real-estate-hunter",
        PluginMetadata {
            role: Role::Career,
            domain: Domain::General,
        },
    ),
    (
        "stock-advisor",
        PluginMetadata {
            role: Role::Career,
            domain: Domain::General,
        },
    ),
];

/// Runtime overlay of ingested-plugin categories, set once at startup from the
/// persisted sidecar (see [`crate::categories`]). Empty until [`set_overlay`] is
/// called, so library tests see only the built-in [`PLUGIN_METADATA`].
static OVERLAY: OnceLock<Vec<(String, PluginMetadata)>> = OnceLock::new();

/// Install the runtime categorization overlay. Call once, early in `main`, with
/// the entries loaded by [`crate::categories::load`]. Later calls are ignored.
pub fn set_overlay(entries: Vec<(String, PluginMetadata)>) {
    let _ = OVERLAY.set(entries);
}

fn overlay() -> &'static [(String, PluginMetadata)] {
    OVERLAY.get().map(Vec::as_slice).unwrap_or(&[])
}

/// The merged catalog: every known plugin and its category. Built-ins come first
/// in [`PLUGIN_METADATA`] order (the overlay may override a built-in's category),
/// then ingested plugins not present among built-ins, sorted by name. This order
/// is the canonical order used everywhere plugins are listed.
pub fn catalog() -> Vec<(String, PluginMetadata)> {
    let mut out: Vec<(String, PluginMetadata)> = PLUGIN_METADATA
        .iter()
        .map(|(name, meta)| {
            let resolved = overlay()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, m)| *m)
                .unwrap_or(*meta);
            (name.to_string(), resolved)
        })
        .collect();

    let mut extra: Vec<(String, PluginMetadata)> = overlay()
        .iter()
        .filter(|(n, _)| !PLUGIN_METADATA.iter().any(|(bn, _)| bn == n))
        .cloned()
        .collect();
    extra.sort_by(|a, b| a.0.cmp(&b.0));
    out.extend(extra);
    out
}

/// Every plugin the CLI knows about, in canonical catalog order.
pub fn all_plugins() -> Vec<String> {
    catalog().into_iter().map(|(name, _)| name).collect()
}

/// Look up a plugin's category by name (built-in const + runtime overlay).
pub fn get_plugin_metadata(name: &str) -> Option<PluginMetadata> {
    catalog()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, m)| m)
}

/// Look up only a *built-in* plugin's category, ignoring the overlay. Used to
/// decide whether an ingested plugin still needs categorizing.
pub fn builtin_metadata(name: &str) -> Option<PluginMetadata> {
    PLUGIN_METADATA
        .iter()
        .find(|(plugin_name, _)| *plugin_name == name)
        .map(|(_, meta)| *meta)
}

/// A named subset of plugins, generated from the catalog by facet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub key: &'static str,
    pub plugins: Vec<String>,
}

/// Expand plugins by facet query over the catalog. Returns catalog members whose
/// role is in `roles` (or any, if empty) AND whose domain is in `domains` (or
/// any, if empty), in canonical order.
fn plugins_by_facets(roles: &[Role], domains: &[Domain]) -> Vec<String> {
    catalog()
        .into_iter()
        .filter(|(_, meta)| {
            (roles.is_empty() || roles.contains(&meta.role))
                && (domains.is_empty() || domains.contains(&meta.domain))
        })
        .map(|(name, _)| name)
        .collect()
}

/// Reduce an arbitrary name set to known catalog members, deduplicated and in
/// canonical order. Names not in the catalog are dropped.
fn canonical(names: &[&str]) -> Vec<String> {
    all_plugins()
        .into_iter()
        .filter(|p| names.contains(&p.as_str()))
        .collect()
}

/// The ordered mode registry. Index in this list (1-based) is the picker number.
///
/// Modes are generated from the catalog by facet so they stay duplicate-free and
/// in sync with categorization: "engineering" modes are the generalist baseline
/// (`ai-architecture` + `software-engineer`) plus every engineer plugin in the
/// given domain(s); role modes are every plugin of a role.
pub fn registry() -> Vec<Mode> {
    fn m(key: &'static str, plugins: Vec<String>) -> Mode {
        Mode { key, plugins }
    }

    // Baseline-plus-engineers-in-domain, in canonical order (baseline included).
    let dev = |domains: &[Domain]| -> Vec<String> {
        let mut want: Vec<String> = vec!["ai-architecture".into(), "software-engineer".into()];
        want.extend(plugins_by_facets(&[Role::Engineer], domains));
        all_plugins()
            .into_iter()
            .filter(|p| want.contains(p))
            .collect()
    };

    vec![
        // Engineering modes by domain (generated from facets).
        m("mobile", dev(&[Domain::Mobile])),
        m("frontend", dev(&[Domain::Web])),
        m("backend", dev(&[Domain::Backend])),
        m("infra", dev(&[Domain::Infra])),
        m(
            "full-stack",
            dev(&[Domain::Web, Domain::Backend, Domain::Infra]),
        ),
        // Role modes.
        m("architect", plugins_by_facets(&[Role::Architect], &[])),
        m(
            "product",
            canonical(&["ai-architecture", "software-engineer", "product"]),
        ),
        m("career", plugins_by_facets(&[Role::Career], &[])),
        m("hobby", plugins_by_facets(&[Role::Hobby], &[])),
        // Niche combinations.
        m("marketing", canonical(&["ai-architecture", "product"])),
        m("jobs", canonical(&["ai-architecture", "job-hunter"])),
        m("music", canonical(&["ai-architecture", "music-librarian"])),
        m(
            "home-devops",
            canonical(&["ai-architecture", "home-assistant", "dev-ops"]),
        ),
        m("oz", canonical(&["ai-architecture", "oz"])),
        m(
            "realestate",
            canonical(&["ai-architecture", "real-estate-hunter"]),
        ),
        m(
            "investments-stock",
            canonical(&["ai-architecture", "stock-advisor"]),
        ),
        m(
            "investments-all",
            canonical(&["ai-architecture", "stock-advisor", "real-estate-hunter"]),
        ),
        // Meta modes.
        m("all", all_plugins()),
        m(
            "minimal",
            canonical(&["ai-architecture", "software-engineer"]),
        ),
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
    /// Chosen mode keys, in selection order.
    pub chosen: Vec<String>,
    /// Union of plugins to enable, in canonical catalog order.
    pub enabled: Vec<String>,
}

impl Resolution {
    /// Returns `(enabled, disabled)` partition over the catalog.
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

    let mut chosen: Vec<String> = Vec::new();
    for tok in tokens {
        let key = resolve_token(tok)?;
        if !chosen.iter().any(|c| c == key) {
            chosen.push(key.to_string());
        }
    }

    // Union of plugins, expressed in canonical catalog order for stable output.
    let mut union: Vec<String> = Vec::new();
    for key in &chosen {
        if let Some(mode) = by_key(key) {
            for p in mode.plugins {
                if !union.contains(&p) {
                    union.push(p);
                }
            }
        }
    }
    let enabled: Vec<String> = all_plugins()
        .into_iter()
        .filter(|p| union.contains(p))
        .collect();

    Ok(Resolution { chosen, enabled })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Owned-string vec, for comparing against the `String`-typed mode output.
    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn registry_has_expected_modes() {
        let keys: Vec<_> = registry().iter().map(|m| m.key).collect();
        assert_eq!(keys[0], "mobile");
        assert_eq!(keys.last().copied(), Some("minimal"));
        assert!(keys.contains(&"investments-all"));
    }

    #[test]
    fn modes_have_unique_plugin_sets() {
        let reg = registry();
        for i in 0..reg.len() {
            for j in (i + 1)..reg.len() {
                assert_ne!(
                    reg[i].plugins, reg[j].plugins,
                    "modes `{}` and `{}` resolve to identical plugins",
                    reg[i].key, reg[j].key
                );
            }
        }
    }

    #[test]
    fn dev_modes_match_facets() {
        // `mobile` is the baseline pair plus every engineer in the mobile domain.
        assert_eq!(
            by_key("mobile").unwrap().plugins,
            names(&["ai-architecture", "software-engineer", "flutter", "android"])
        );
        assert_eq!(
            by_key("architect").unwrap().plugins,
            names(&["ai-architecture"])
        );
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
                assert!(known.contains(&p), "{} -> unknown plugin {}", mode.key, p);
            }
        }
    }

    #[test]
    fn resolve_by_name() {
        let r = resolve("mobile").unwrap();
        assert_eq!(r.chosen, names(&["mobile"]));
        // Mobile mode: architect + engineer + flutter + android (canonical catalog order)
        assert_eq!(
            r.enabled,
            names(&["ai-architecture", "software-engineer", "flutter", "android"])
        );
    }

    #[test]
    fn resolve_by_index() {
        // 1 == mobile
        assert_eq!(resolve("1").unwrap().chosen, names(&["mobile"]));
    }

    #[test]
    fn resolve_multiple_unions_in_canonical_order() {
        let r = resolve("frontend product").unwrap();
        assert_eq!(r.chosen, names(&["frontend", "product"]));
        // Union of frontend + product in canonical order
        assert_eq!(
            r.enabled,
            names(&[
                "ai-architecture",
                "software-engineer",
                "frontend",
                "frontend-native",
                "product"
            ])
        );
    }

    #[test]
    fn resolve_comma_and_index_mix() {
        let r = resolve("mobile, jobs").unwrap(); // mobile + jobs
        assert_eq!(r.chosen, names(&["mobile", "jobs"]));
        assert!(r.enabled.iter().any(|p| p == "job-hunter"));
        assert!(r.enabled.iter().any(|p| p == "flutter"));
    }

    #[test]
    fn resolve_dedups_repeated_selection() {
        let r = resolve("mobile mobile 1").unwrap();
        assert_eq!(r.chosen, names(&["mobile"]));
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
        assert_eq!(on, names(&["ai-architecture", "software-engineer"]));
        assert_eq!(on.len() + off.len(), all_plugins().len());
    }
}
