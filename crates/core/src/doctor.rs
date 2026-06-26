//! The `doctor` diagnostic: a read-only snapshot of how plugins line up across
//! the project marker, the `.aip-cli` store, and each installed AI agent.
//!
//! All logic here is pure: the CLI gathers the raw inputs (filesystem reads,
//! done in parallel) and hands them to [`build_report`]; [`render`] turns the
//! report into the text the user sees. Both are unit-tested without any I/O.

use crate::agent_state::AgentPlugins;
use crate::hook::{content_hash, Marker, MarkerTarget};
use crate::manifest::PluginManifest;
use crate::mode_apply::Target;
use crate::modes;
use crate::store::is_plugin_dir;
use std::path::{Path, PathBuf};

// ─── inputs (gathered by the CLI) ────────────────────────────────────────────

/// A plugin held in the `.aip-cli` store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePlugin {
    pub name: String,
    pub version: String,
}

/// Scan the `.aip-cli` store at `plugins_root` into the list `build_report`
/// cross-references, sorted by plugin name.
///
/// Each entry is keyed by its **manifest name**, not its directory name: the
/// store folder for a plugin can differ from the name agents and modes use
/// (e.g. the `android` folder holds the `android-native` plugin), and keying by
/// the folder name would make such plugins look missing from the store. A
/// directory whose manifest is unreadable falls back to its directory name with
/// an unknown (`"?"`) version so it still shows up rather than silently
/// vanishing.
pub fn scan_store(plugins_root: &Path) -> Vec<StorePlugin> {
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(plugins_root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && is_plugin_dir(p))
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    let mut out: Vec<StorePlugin> = dirs
        .iter()
        .map(|d| match PluginManifest::read(d) {
            Ok(m) => StorePlugin {
                name: m.name,
                version: m.version,
            },
            Err(_) => StorePlugin {
                name: d
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                version: "?".to_string(),
            },
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Raw per-agent input: whether it's on `PATH` and its parsed on-disk state.
#[derive(Debug, Clone)]
pub struct AgentInput {
    pub target: Target,
    pub on_path: bool,
    pub state: Option<AgentPlugins>,
}

/// Raw project input: the marker in scope (if any), its trust, and the last
/// applied hash.
#[derive(Debug, Clone, Default)]
pub struct ProjectInput {
    pub marker: Option<PathBuf>,
    pub marker_text: Option<String>,
    pub trusted: bool,
    pub applied_hash: Option<String>,
}

// ─── report ──────────────────────────────────────────────────────────────────

/// Project-folder status derived from the `.aip-cli.toml` marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStatus {
    /// Absolute marker path, if one was found in scope.
    pub marker: Option<PathBuf>,
    /// Raw mode selector from the marker.
    pub mode: Option<String>,
    /// Resolved mode keys (empty when the selector is invalid).
    pub chosen: Vec<String>,
    /// Agent the marker pins to, or `None` for "all installed agents".
    pub pinned: Option<Target>,
    /// Whether the marker is trusted at its current content hash.
    pub trusted: bool,
    /// Whether the marker's mode has already been applied (active).
    pub active: bool,
    /// Plugins the marker's mode resolves to, in canonical order.
    pub wants: Vec<String>,
    /// Set when a marker exists but its mode can't be parsed/resolved.
    pub error: Option<String>,
}

/// Per-agent status, cross-referenced against the store and the project mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub target: Target,
    /// On `PATH`.
    pub installed: bool,
    /// A state config file was found and parsed.
    pub configured: bool,
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    /// Enabled plugins not present in the `.aip-cli` store.
    pub orphans: Vec<String>,
    /// Plugins the project mode wants enabled here but that aren't.
    pub missing_wanted: Vec<String>,
    /// True when this agent's managed-plugin state diverges from what the
    /// project mode wants (only when the mode targets this agent).
    pub drift: bool,
}

/// The full doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub project: ProjectStatus,
    pub store: Vec<StorePlugin>,
    pub agents: Vec<AgentStatus>,
    /// Plugins the project mode wants that the store doesn't hold.
    pub wants_missing_from_store: Vec<String>,
}

/// Does the marker apply to `target`? `None` pin means every agent; a pin means
/// only that agent.
fn mode_targets(pinned: Option<Target>, target: Target) -> bool {
    pinned.map(|p| p == target).unwrap_or(true)
}

/// Assemble a [`DoctorReport`] from gathered inputs. Pure.
pub fn build_report(
    project: ProjectInput,
    store: Vec<StorePlugin>,
    agents: Vec<AgentInput>,
) -> DoctorReport {
    let store_names: Vec<&str> = store.iter().map(|p| p.name.as_str()).collect();

    // ── project ──
    let mut mode = None;
    let mut chosen = Vec::new();
    let mut pinned = None;
    let mut wants: Vec<String> = Vec::new();
    let mut error = None;
    let mut active = false;

    if let Some(text) = &project.marker_text {
        match Marker::parse(text) {
            Ok(m) => {
                mode = Some(m.mode.clone());
                pinned = m.target.map(|t: MarkerTarget| t.into());
                match modes::resolve(&m.mode) {
                    Ok(res) => {
                        chosen = res.chosen.iter().map(|s| s.to_string()).collect();
                        wants = res.enabled.iter().map(|s| s.to_string()).collect();
                    }
                    Err(e) => error = Some(e.to_string()),
                }
            }
            Err(e) => error = Some(e.to_string()),
        }
        // Active only when trusted and the applied hash matches this marker.
        if project.trusted {
            active = project.applied_hash.as_deref() == Some(content_hash(text).as_str());
        }
    }

    let project_status = ProjectStatus {
        marker: project.marker,
        mode,
        chosen,
        pinned,
        trusted: project.trusted,
        active,
        wants: wants.clone(),
        error,
    };

    // ── agents ──
    let agents = agents
        .into_iter()
        .map(|a| {
            let configured = a.state.is_some();
            let state = a.state.unwrap_or_default();
            let orphans: Vec<String> = state
                .enabled
                .iter()
                .filter(|p| !store_names.contains(&p.as_str()))
                .cloned()
                .collect();

            // Only compare against the project mode when it actually targets
            // this agent and resolved to a plugin set.
            let targeted = !wants.is_empty() && mode_targets(project_status.pinned, a.target);
            let missing_wanted: Vec<String> = if targeted {
                wants
                    .iter()
                    .filter(|p| !state.is_enabled(p))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            // Drift over the managed plugin universe only, so agent-specific
            // extras (e.g. plugins outside any mode) never count as drift.
            let drift = targeted
                && modes::all_plugins().iter().any(|p| {
                    let want = wants.iter().any(|w| w == p);
                    want != state.is_enabled(p)
                });

            AgentStatus {
                target: a.target,
                installed: a.on_path,
                configured,
                enabled: state.enabled,
                disabled: state.disabled,
                orphans,
                missing_wanted,
                drift,
            }
        })
        .collect();

    let wants_missing_from_store: Vec<String> = wants
        .iter()
        .filter(|p| !store_names.contains(&p.as_str()))
        .cloned()
        .collect();

    DoctorReport {
        project: project_status,
        store,
        agents,
        wants_missing_from_store,
    }
}

// ─── rendering ───────────────────────────────────────────────────────────────

const OK: &str = "✓";
const WARN: &str = "⚠";
const OFF: &str = "–";

/// Render the report to the text the user sees.
pub fn render(r: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("aip-cli doctor\n");

    render_project(r, &mut out);
    render_store(r, &mut out);
    render_agents(r, &mut out);

    out
}

fn render_project(r: &DoctorReport, out: &mut String) {
    let p = &r.project;
    out.push_str("\nProject (current folder)\n");
    match &p.marker {
        None => {
            out.push_str(&format!(
                "  {OFF} no .aip-cli.toml marker in scope (run `aip-cli init <mode>`)\n"
            ));
            return;
        }
        Some(m) => out.push_str(&format!("  {OK} marker: {}\n", m.display())),
    }

    if let Some(err) = &p.error {
        out.push_str(&format!("  {WARN} invalid mode: {err}\n"));
    } else if let Some(mode) = &p.mode {
        out.push_str(&format!(
            "  {} mode: {} → {}\n",
            OK,
            mode,
            if p.chosen.is_empty() {
                "(none)".to_string()
            } else {
                p.chosen.join(", ")
            }
        ));
    }

    let scope = match p.pinned {
        Some(t) => format!("pinned to {}", t.key()),
        None => "all installed agents".to_string(),
    };
    out.push_str(&format!("  {OK} applies to: {scope}\n"));

    let (icon, trust) = if p.trusted {
        (OK, "trusted")
    } else {
        (
            WARN,
            "untrusted — run `aip-cli allow` to enable auto-activation",
        )
    };
    out.push_str(&format!("  {icon} trust: {trust}\n"));

    if p.error.is_none() && p.mode.is_some() {
        let (icon, state) = if p.active {
            (OK, "active (mode applied)")
        } else {
            (
                WARN,
                "pending (mode not yet applied — `cd` here or run `aip-cli mode`)",
            )
        };
        out.push_str(&format!("  {icon} state: {state}\n"));

        if p.wants.is_empty() {
            out.push_str(&format!("  {OFF} enables no plugins\n"));
        } else {
            out.push_str(&format!("  {OK} enables: {}\n", p.wants.join(", ")));
        }
        if !r.wants_missing_from_store.is_empty() {
            out.push_str(&format!(
                "  {WARN} not in store: {} (ingest them, then `aip-cli setup`)\n",
                r.wants_missing_from_store.join(", ")
            ));
        }
    }
}

fn render_store(r: &DoctorReport, out: &mut String) {
    out.push_str(&format!(
        "\nStore (~/.aip-cli/plugins) — {}\n",
        r.store.len()
    ));
    if r.store.is_empty() {
        out.push_str(&format!(
            "  {OFF} empty (ingest with `aip-cli ingest-folder <dir>` or `ingest-url <url>`)\n"
        ));
        return;
    }
    for p in &r.store {
        out.push_str(&format!("  {OK} {} ({})\n", p.name, p.version));
    }
}

fn render_agents(r: &DoctorReport, out: &mut String) {
    out.push_str("\nAI CLIs\n");
    for a in &r.agents {
        if !a.installed {
            out.push_str(&format!(
                "  {OFF} {} — not installed (not on PATH)\n",
                a.target.key()
            ));
            continue;
        }
        if !a.configured {
            out.push_str(&format!(
                "  {WARN} {} — installed, no plugin config yet\n",
                a.target.key()
            ));
            continue;
        }
        out.push_str(&format!(
            "  {OK} {} — {} enabled, {} disabled\n",
            a.target.key(),
            a.enabled.len(),
            a.disabled.len()
        ));
        if !a.enabled.is_empty() {
            out.push_str(&format!("      enabled: {}\n", a.enabled.join(", ")));
        }
        if !a.orphans.is_empty() {
            out.push_str(&format!(
                "      {WARN} enabled but not in store: {}\n",
                a.orphans.join(", ")
            ));
        }
        if a.drift {
            if a.missing_wanted.is_empty() {
                out.push_str(&format!(
                    "      {WARN} drift from project mode (extra plugins enabled)\n"
                ));
            } else {
                out.push_str(&format!(
                    "      {WARN} project mode wants but disabled here: {}\n",
                    a.missing_wanted.join(", ")
                ));
            }
        } else if !r.project.wants.is_empty() && mode_targets(r.project.pinned, a.target) {
            out.push_str(&format!("      {OK} matches project mode\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(names: &[&str]) -> Vec<StorePlugin> {
        names
            .iter()
            .map(|n| StorePlugin {
                name: n.to_string(),
                version: "1.0.0".into(),
            })
            .collect()
    }

    fn agent(target: Target, on_path: bool, enabled: &[&str], disabled: &[&str]) -> AgentInput {
        AgentInput {
            target,
            on_path,
            state: Some(AgentPlugins {
                enabled: enabled.iter().map(|s| s.to_string()).collect(),
                disabled: disabled.iter().map(|s| s.to_string()).collect(),
            }),
        }
    }

    /// Write a minimal plugin into `root/dir` with the given manifest name and
    /// version. The directory name and manifest name are intentionally allowed
    /// to differ (as `android` vs `android-native` in the real store).
    fn write_plugin(root: &std::path::Path, dir: &str, name: &str, version: &str) {
        let meta = root.join(dir).join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn no_marker_reports_clean_project() {
        let r = build_report(ProjectInput::default(), store(&[]), vec![]);
        assert!(r.project.marker.is_none());
        assert!(r.project.wants.is_empty());
        assert!(render(&r).contains("no .aip-cli.toml marker"));
    }

    #[test]
    fn resolves_marker_mode_to_wanted_plugins() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/repo/.aip-cli.toml")),
            marker_text: Some("mode = \"minimal\"".into()),
            trusted: true,
            applied_hash: None,
        };
        let r = build_report(
            proj,
            store(&["ai-architecture", "software-engineer"]),
            vec![],
        );
        assert_eq!(r.project.mode.as_deref(), Some("minimal"));
        assert_eq!(r.project.chosen, vec!["minimal"]);
        assert_eq!(
            r.project.wants,
            vec!["ai-architecture", "software-engineer"]
        );
        assert!(r.wants_missing_from_store.is_empty());
    }

    #[test]
    fn flags_wanted_plugin_missing_from_store() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some("mode = \"minimal\"".into()),
            trusted: true,
            applied_hash: None,
        };
        // store lacks software-engineer
        let r = build_report(proj, store(&["ai-architecture"]), vec![]);
        assert_eq!(r.wants_missing_from_store, vec!["software-engineer"]);
        assert!(render(&r).contains("not in store"));
    }

    #[test]
    fn active_when_applied_hash_matches() {
        let text = "mode = \"minimal\"".to_string();
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some(text.clone()),
            trusted: true,
            applied_hash: Some(content_hash(&text)),
        };
        let r = build_report(proj, store(&[]), vec![]);
        assert!(r.project.active);
        assert!(render(&r).contains("active (mode applied)"));
    }

    #[test]
    fn not_active_when_untrusted_even_if_hash_matches() {
        let text = "mode = \"minimal\"".to_string();
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some(text.clone()),
            trusted: false,
            applied_hash: Some(content_hash(&text)),
        };
        let r = build_report(proj, store(&[]), vec![]);
        assert!(!r.project.active);
    }

    #[test]
    fn invalid_mode_records_error() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some("mode = \"nope\"".into()),
            trusted: true,
            applied_hash: None,
        };
        let r = build_report(proj, store(&[]), vec![]);
        assert!(r.project.error.is_some());
        assert!(render(&r).contains("invalid mode"));
    }

    #[test]
    fn agent_not_installed_is_reported() {
        let agents = vec![AgentInput {
            target: Target::Grok,
            on_path: false,
            state: None,
        }];
        let r = build_report(ProjectInput::default(), store(&[]), agents);
        assert!(!r.agents[0].installed);
        assert!(render(&r).contains("grok — not installed"));
    }

    #[test]
    fn orphan_enabled_plugin_not_in_store_is_flagged() {
        let agents = vec![agent(Target::Grok, true, &["it-manager"], &[])];
        let r = build_report(ProjectInput::default(), store(&["ai-architecture"]), agents);
        assert_eq!(r.agents[0].orphans, vec!["it-manager"]);
        assert!(render(&r).contains("enabled but not in store: it-manager"));
    }

    #[test]
    fn drift_detected_when_agent_missing_a_wanted_plugin() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some("mode = \"minimal\"".into()),
            trusted: true,
            applied_hash: None,
        };
        // minimal wants ai-architecture + software-engineer; agent has only one.
        let agents = vec![agent(
            Target::ClaudeCode,
            true,
            &["ai-architecture"],
            &["software-engineer"],
        )];
        let r = build_report(
            proj,
            store(&["ai-architecture", "software-engineer"]),
            agents,
        );
        assert!(r.agents[0].drift);
        assert_eq!(r.agents[0].missing_wanted, vec!["software-engineer"]);
    }

    #[test]
    fn no_drift_when_agent_matches_mode_exactly() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some("mode = \"minimal\"".into()),
            trusted: true,
            applied_hash: None,
        };
        let agents = vec![agent(
            Target::ClaudeCode,
            true,
            &["ai-architecture", "software-engineer"],
            &["flutter-pivara"],
        )];
        let r = build_report(proj, store(&[]), agents);
        assert!(!r.agents[0].drift);
        assert!(render(&r).contains("matches project mode"));
    }

    #[test]
    fn pinned_marker_skips_drift_for_other_agent() {
        let proj = ProjectInput {
            marker: Some(PathBuf::from("/m")),
            marker_text: Some("mode = \"minimal\"\ntarget = \"grok\"".into()),
            trusted: true,
            applied_hash: None,
        };
        // Claude has nothing enabled, but the marker pins grok, so no drift.
        let agents = vec![agent(Target::ClaudeCode, true, &[], &[])];
        let r = build_report(proj, store(&[]), agents);
        assert_eq!(r.project.pinned, Some(Target::Grok));
        assert!(!r.agents[0].drift);
        assert!(r.agents[0].missing_wanted.is_empty());
    }

    #[test]
    fn scan_store_keys_by_manifest_name_not_dir_name() {
        // Regression: the `android` folder holds the `android-native` plugin and
        // `flutter` holds `flutter-pivara`. The store list must be keyed by the
        // manifest name (what agents and modes use), not the directory name —
        // otherwise doctor reports these plugins as missing/orphaned.
        let tmp = tempfile::TempDir::new().unwrap();
        write_plugin(tmp.path(), "android", "android-native", "1.0.1");
        write_plugin(tmp.path(), "flutter", "flutter-pivara", "1.1.0");
        write_plugin(tmp.path(), "product", "product", "2.0.0");
        std::fs::create_dir_all(tmp.path().join("not-a-plugin")).unwrap();

        let scanned = scan_store(tmp.path());
        let names: Vec<&str> = scanned.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["android-native", "flutter-pivara", "product"]);
        assert_eq!(scanned[0].version, "1.0.1");
    }

    #[test]
    fn scanned_store_makes_enabled_plugin_not_an_orphan() {
        // End-to-end: a plugin enabled under its manifest name must not be
        // flagged as an orphan just because its store folder is named differently.
        let tmp = tempfile::TempDir::new().unwrap();
        write_plugin(tmp.path(), "android", "android-native", "1.0.1");

        let agents = vec![agent(Target::ClaudeCode, true, &["android-native"], &[])];
        let r = build_report(ProjectInput::default(), scan_store(tmp.path()), agents);
        assert!(
            r.agents[0].orphans.is_empty(),
            "android-native is in the store (folder `android`) and must not be an orphan"
        );
    }

    #[test]
    fn scan_store_falls_back_to_dir_name_on_unreadable_manifest() {
        // A plugin dir whose manifest is present but malformed still appears,
        // keyed by its directory name with an unknown version, instead of vanishing.
        let tmp = tempfile::TempDir::new().unwrap();
        let meta = tmp.path().join("broken").join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("plugin.json"), "{not json").unwrap();

        let scanned = scan_store(tmp.path());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].name, "broken");
        assert_eq!(scanned[0].version, "?");
    }
}
