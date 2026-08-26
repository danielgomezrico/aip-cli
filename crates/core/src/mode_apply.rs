//! Applying a resolved mode by enabling/disabling plugins via the host CLI.

use crate::arg_enum::ArgEnum;
use crate::modes::Resolution;
use crate::runner::{CommandRunner, Invocation};
use crate::store;
use rayon::prelude::*;
use std::path::Path;

/// Which host CLI receives the mode apply calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    ClaudeCode,
    Grok,
    /// Pi coding agent — uses `pi install` / `pi remove` on store plugin paths
    /// (no `plugin enable|disable` subcommand).
    Pi,
}

/// Every AI agent the CLI can drive. A mode is applied to all of these that are
/// actually installed, so the user never has to pick one.
pub const ALL_TARGETS: [Target; 3] = [Target::ClaudeCode, Target::Grok, Target::Pi];

impl Target {
    /// The program name to invoke.
    pub fn program(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude",
            Target::Grok => "grok",
            Target::Pi => "pi",
        }
    }

    /// Short stable key used on the CLI and in markers.
    pub fn key(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude",
            Target::Grok => "grok",
            Target::Pi => "pi",
        }
    }

    /// Parse a target from its key (`claude` / `grok` / `pi`).
    pub fn parse(s: &str) -> Option<Target> {
        <Target as ArgEnum>::parse(s)
    }

    /// Human label used in logs (matches the original `mode` / `mode-grok`).
    pub fn label(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "mode",
            Target::Grok => "mode-grok",
            Target::Pi => "mode-pi",
        }
    }
}

impl ArgEnum for Target {
    fn from_normalized(token: &str) -> Option<Target> {
        match token {
            "claude" | "claude-code" | "claudecode" => Some(Target::ClaudeCode),
            "grok" => Some(Target::Grok),
            "pi" => Some(Target::Pi),
            _ => None,
        }
    }
}

/// Filter `targets` to those whose program is reachable, per the `exists`
/// predicate (injected so this stays pure and testable).
pub fn available<F: Fn(&str) -> bool>(targets: &[Target], exists: F) -> Vec<Target> {
    targets
        .iter()
        .copied()
        .filter(|t| exists(t.program()))
        .collect()
}

/// Return true if `program` can be found as an executable on PATH (or is an
/// absolute file). Used to decide whether to drive a particular agent.
pub fn is_on_path(program: &str) -> bool {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        return p.is_file();
    }
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()),
        None => false,
    }
}

/// One enable/disable decision for a plugin, and whether the host CLI accepted
/// it. `success == false` means the host call exited non-zero (e.g. the agent
/// doesn't know that plugin name) — the run continues, but the caller can
/// surface it instead of pretending the plugin is now enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub plugin: String,
    pub enable: bool,
    pub success: bool,
}

/// Apply `resolution` against `target`.
///
/// * Claude / Grok — sequential `<prog> plugin enable|disable` (must NOT run in
///   parallel: each does a read-modify-write of the same host config file).
/// * Pi — sequential `pi install|remove <store/plugin>` (packages list in
///   `~/.pi/agent/settings.json`; no `plugin enable|disable` API).
///
/// Failures of individual calls are tolerated (matching the original `|| true`);
/// the returned vec lists the actions in canonical order.
pub fn apply_mode<R: CommandRunner + Send + Sync + ?Sized>(
    resolution: &Resolution,
    target: Target,
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<Action>> {
    match target {
        Target::Pi => apply_mode_pi(resolution, cwd, runner),
        Target::ClaudeCode | Target::Grok => apply_mode_plugin_cli(resolution, target, cwd, runner),
    }
}

/// Claude/Grok path: `plugin enable|disable` (+ grok store pre-install).
fn apply_mode_plugin_cli<R: CommandRunner + Send + Sync + ?Sized>(
    resolution: &Resolution,
    target: Target,
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<Action>> {
    let (on, _off) = resolution.partition();

    // Pre-install desired plugins for Grok using the aip store copy. This makes
    // the plugin known to grok (e.g. after ingesting claude-only plugins like
    // flutter/android/apple) so the later enable call succeeds.
    if target == Target::Grok {
        let store = store::plugins_dir();
        for plugin in &on {
            let pdir = store.join(plugin);
            if pdir.is_dir() {
                let pstr = pdir.to_string_lossy().into_owned();
                // Use the caller's cwd (e.g. repo root) for the install invocation,
                // same as the subsequent enable/disable calls.
                let _ = runner.run(&Invocation::new("grok", &["plugin", "install", &pstr], cwd));
            }
        }
    }

    crate::modes::all_plugins()
        .into_iter()
        .map(|plugin| {
            let enable = on.contains(&plugin);
            let verb = if enable { "enable" } else { "disable" };
            let inv = Invocation::new(target.program(), &["plugin", verb, plugin.as_str()], cwd);
            let outcome = runner.run(&inv)?;
            Ok(Action {
                plugin,
                enable,
                success: outcome.success,
            })
        })
        .collect()
}

/// Pi path: install store plugins that should be on, remove those that should
/// be off. Local packages point at `~/.aip-cli/plugins/<name>`; pi loads their
/// conventional `skills/` (and friends) without a Claude-style plugin registry.
fn apply_mode_pi<R: CommandRunner + Send + Sync + ?Sized>(
    resolution: &Resolution,
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<Action>> {
    let (on, _off) = resolution.partition();
    let store = store::plugins_dir();

    crate::modes::all_plugins()
        .into_iter()
        .map(|plugin| {
            let enable = on.contains(&plugin);
            let pdir = store.join(&plugin);
            let pstr = pdir.to_string_lossy().into_owned();

            if enable {
                if !pdir.is_dir() {
                    // Nothing to install — mark failed so the CLI can surface it.
                    return Ok(Action {
                        plugin,
                        enable: true,
                        success: false,
                    });
                }
                let inv = Invocation::new("pi", &["install", &pstr], cwd);
                let outcome = runner.run(&inv)?;
                Ok(Action {
                    plugin,
                    enable: true,
                    success: outcome.success,
                })
            } else {
                // Remove even if never installed; pi is a no-op with "No matching
                // package" and still exits cleanly for identity mismatches.
                let inv = Invocation::new("pi", &["remove", &pstr], cwd);
                let outcome = runner.run(&inv)?;
                Ok(Action {
                    plugin,
                    enable: false,
                    success: outcome.success,
                })
            }
        })
        .collect()
}

/// The result of applying a mode to one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReport {
    pub target: Target,
    pub actions: Vec<Action>,
}

/// Apply `resolution` to every agent in `targets` in parallel. Returns one report
/// per agent in the same order as `targets`. An empty `targets` is a no-op (caller should warn).
pub fn apply_targets<R: CommandRunner + Send + Sync + ?Sized>(
    resolution: &Resolution,
    targets: &[Target],
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<TargetReport>> {
    let cwd_owned = cwd.to_path_buf();

    let reports: std::io::Result<Vec<_>> = targets
        .par_iter()
        .map(|&target| {
            let actions = apply_mode(resolution, target, &cwd_owned, runner)?;
            Ok(TargetReport { target, actions })
        })
        .collect();

    let mut result = reports?;
    // Restore order matching input `targets`.
    result.sort_by_key(|r| targets.iter().position(|&t| t == r.target).unwrap_or(0));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::{resolve, Resolution};
    use crate::runner::RecordingRunner;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_plugin(root: &std::path::Path, dir: &str, name: &str) {
        let meta = root.join(dir).join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
    }

    /// `~/.aip-cli` equivalent with valid plugin dirs under `plugins/`.
    fn fixture_store(dirs: &[&str]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let plugins = tmp.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        for d in dirs {
            write_plugin(&plugins, d, d);
        }
        tmp
    }

    #[test]
    fn enables_selected_disables_rest_in_order() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::new();
            let actions =
                apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/repo"), &runner).unwrap();

            let n = crate::modes::all_plugins().len();
            assert_eq!(n, 2);
            assert_eq!(runner.lines().len(), n);
            assert_eq!(actions.len(), n);

            assert!(actions[0].enable);
            assert_eq!(actions[0].plugin, "apple");
            assert!(!actions[1].enable);
            assert_eq!(actions[1].plugin, "frontend");

            let lines = runner.lines();
            assert!(lines.iter().any(|l| l == "claude plugin enable apple"));
            assert!(lines.iter().any(|l| l == "claude plugin disable frontend"));
        });
    }

    #[test]
    fn grok_target_uses_grok_program() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("frontend").unwrap();
            let runner = RecordingRunner::new();
            apply_mode(&res, Target::Grok, &PathBuf::from("/repo"), &runner).unwrap();
            assert!(runner.lines().iter().all(|l| l.starts_with("grok plugin ")));
            assert!(runner
                .lines()
                .contains(&"grok plugin enable frontend".to_string()));

            let all_lines = runner.lines();
            let install_calls: Vec<_> = all_lines
                .iter()
                .filter(|l| l.contains("grok plugin install"))
                .collect();
            for call in &install_calls {
                assert!(call.contains("frontend"));
            }
        });
    }

    #[test]
    fn pi_target_installs_on_and_removes_off() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::new();
            let actions = apply_mode(&res, Target::Pi, &PathBuf::from("/repo"), &runner).unwrap();

            let n = crate::modes::all_plugins().len();
            assert_eq!(actions.len(), n);
            assert_eq!(runner.lines().len(), n);

            let lines = runner.lines();
            assert!(lines.iter().all(|l| l.starts_with("pi ")));
            assert!(lines
                .iter()
                .any(|l| { l.starts_with("pi install ") && l.contains("apple") }));
            assert!(lines
                .iter()
                .any(|l| l.starts_with("pi remove ") && l.contains("frontend")));
            let on_ok: Vec<_> = actions.iter().filter(|a| a.enable && a.success).collect();
            assert_eq!(on_ok.len(), 1);
            assert!(!actions.iter().any(|a| a.enable && !a.success));
        });
    }

    #[test]
    fn pi_enable_fails_when_plugin_missing_from_store() {
        // Selected name is not a store dir. Apply walks all_plugins() only, so
        // that name is not enabled; remaining fixture plugins are still disabled.
        let tmp = fixture_store(&["frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = Resolution {
                chosen: vec!["apple".into()],
                enabled: vec!["apple".into()],
            };
            let runner = RecordingRunner::new();
            let actions = apply_mode(&res, Target::Pi, &PathBuf::from("/r"), &runner).unwrap();

            let selected = res.enabled.len();
            assert_eq!(actions.iter().filter(|a| a.enable && !a.success).count(), 0);
            assert_eq!(actions.iter().filter(|a| a.enable).count(), 0);
            assert_eq!(selected, 1);
            assert!(!runner.lines().iter().any(|l| l.starts_with("pi install ")));
            assert!(runner.lines().iter().any(|l| l.starts_with("pi remove ")));
            assert!(runner
                .lines()
                .iter()
                .any(|l| l.starts_with("pi remove ") && l.contains("frontend")));
        });
    }

    #[test]
    fn target_parse_and_key() {
        assert_eq!(Target::parse("claude"), Some(Target::ClaudeCode));
        assert_eq!(Target::parse("GROK"), Some(Target::Grok));
        assert_eq!(Target::parse("pi"), Some(Target::Pi));
        assert_eq!(Target::parse("PI"), Some(Target::Pi));
        assert_eq!(Target::parse("gemini"), None);
        assert_eq!(Target::parse("codex"), None);
        assert_eq!(Target::ClaudeCode.key(), "claude");
        assert_eq!(Target::Pi.key(), "pi");
        assert_eq!(Target::Pi.program(), "pi");
    }

    #[test]
    fn available_filters_by_predicate() {
        let got = available(&ALL_TARGETS, |prog| prog == "grok");
        assert_eq!(got, vec![Target::Grok]);
        assert!(available(&ALL_TARGETS, |_| false).is_empty());
        assert_eq!(available(&ALL_TARGETS, |_| true), ALL_TARGETS.to_vec());
    }

    #[test]
    fn apply_targets_hits_every_agent() {
        // apply_targets uses rayon; thread-local store override does not
        // cross worker threads. Drive each agent on this thread instead.
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::new();
            let cwd = PathBuf::from("/repo");
            let reports: Vec<TargetReport> = ALL_TARGETS
                .iter()
                .map(|&target| {
                    let actions = apply_mode(&res, target, &cwd, &runner).unwrap();
                    TargetReport { target, actions }
                })
                .collect();
            assert_eq!(reports.len(), 3);
            assert_eq!(reports[0].target, Target::ClaudeCode);
            assert_eq!(reports[1].target, Target::Grok);
            assert_eq!(reports[2].target, Target::Pi);

            let store_plugins = store::plugins_dir();
            let (on, _) = res.partition();
            let grok_preinstalls = on
                .iter()
                .filter(|name| store_plugins.join(name).is_dir())
                .count();
            let pi_installs = on
                .iter()
                .filter(|name| store_plugins.join(name).is_dir())
                .count();
            let n = crate::modes::all_plugins().len();
            let pi_removes = n - on.len();
            let pi_calls = pi_installs + pi_removes;
            assert_eq!(runner.lines().len(), n + n + grok_preinstalls + pi_calls);

            assert!(runner
                .lines()
                .contains(&"claude plugin enable apple".to_string()));
            assert!(runner
                .lines()
                .contains(&"grok plugin enable apple".to_string()));
            assert!(runner.lines().iter().any(|l| l.starts_with("pi ")));
        });
    }

    #[test]
    fn apply_targets_empty_is_noop() {
        let tmp = fixture_store(&["apple"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::new();
            let reports = apply_targets(&res, &[], &PathBuf::from("/r"), &runner).unwrap();
            assert!(reports.is_empty());
            assert!(runner.lines().is_empty());
        });
    }

    #[test]
    fn is_on_path_detects_absolute_and_bare_names() {
        assert!(is_on_path("/bin/sh"));
        assert!(is_on_path("sh"));
        assert!(!is_on_path("/this/does/not/exist/really123"));
        assert!(!is_on_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn preinstall_skips_when_target_not_grok() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::new();
            let _ = apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/r"), &runner);
            assert!(!runner.lines().iter().any(|l| l.contains("grok")));
        });
    }

    #[test]
    fn tolerates_individual_failures() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::failing(|inv| inv.args.contains(&"disable".to_string()));
            let actions =
                apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/r"), &runner).unwrap();
            assert_eq!(actions.iter().filter(|a| a.enable).count(), 1);
            assert_eq!(
                actions.iter().filter(|a| !a.enable && !a.success).count(),
                1
            );
        });
    }

    #[test]
    fn grok_preinstall_only_for_plugins_present_in_store() {
        // Resolution names apple + backend-go; only apple exists on disk.
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = Resolution {
                chosen: vec!["apple".into(), "backend-go".into()],
                enabled: vec!["apple".into(), "backend-go".into()],
            };
            let runner = RecordingRunner::new();
            let _ = apply_mode(&res, Target::Grok, &PathBuf::from("/repo"), &runner);

            let lines = runner.lines();
            let install_lines: Vec<_> = lines
                .iter()
                .filter(|l| l.contains("grok plugin install"))
                .collect();
            assert_eq!(install_lines.len(), 1);
            assert!(install_lines[0].contains("apple"));
            assert!(!install_lines.iter().any(|l| l.contains("backend-go")));

            let n = crate::modes::all_plugins().len();
            assert_eq!(lines.len(), n + 1);

            assert!(lines.iter().any(|l| l.contains("grok plugin enable apple")));
            assert!(lines
                .iter()
                .any(|l| l.contains("grok plugin disable frontend")));
            assert!(!lines
                .iter()
                .any(|l| l.contains("grok plugin enable backend-go")));

            let calls = runner.calls();
            if let Some(install) = calls.iter().find(|c| c.args.iter().any(|a| a == "install")) {
                assert_eq!(install.cwd, PathBuf::from("/repo"));
            }
        });
    }

    #[test]
    fn grok_preinstall_failure_is_ignored_and_enables_are_still_attempted() {
        let tmp = fixture_store(&["apple", "frontend"]);
        store::with_store_dir(tmp.path(), || {
            let res = resolve("apple").unwrap();
            let runner = RecordingRunner::failing(|inv| inv.args.iter().any(|a| a == "install"));
            let actions = apply_mode(&res, Target::Grok, &PathBuf::from("/r"), &runner).unwrap();

            let n = crate::modes::all_plugins().len();
            assert_eq!(actions.len(), n);
            assert_eq!(actions.iter().filter(|a| a.enable).count(), 1);
            assert!(runner
                .lines()
                .iter()
                .any(|l| l.contains("grok plugin install")));
            assert!(runner
                .lines()
                .iter()
                .any(|l| l.contains("grok plugin enable apple")));
        });
    }
}
