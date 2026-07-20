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
    use crate::modes::resolve;
    use crate::runner::RecordingRunner;
    use std::path::PathBuf;

    #[test]
    fn enables_selected_disables_rest_in_order() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let actions =
            apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/repo"), &runner).unwrap();

        // One call per plugin.
        assert_eq!(runner.lines().len(), crate::modes::all_plugins().len());
        assert_eq!(actions.len(), crate::modes::all_plugins().len());

        // Actions are returned in canonical order.
        assert_eq!(actions[0].enable, true);
        assert_eq!(actions[0].plugin, "ai-architecture");
        assert_eq!(actions[1].enable, true);
        assert_eq!(actions[1].plugin, "software-engineer");

        // Check runner was called for each plugin (order may vary due to parallel execution).
        let lines = runner.lines();
        assert!(lines
            .iter()
            .any(|l| l == "claude plugin enable ai-architecture"));
        assert!(lines
            .iter()
            .any(|l| l == "claude plugin enable software-engineer"));
        assert!(lines.iter().any(|l| l == "claude plugin disable flutter"));
    }

    #[test]
    fn grok_target_uses_grok_program() {
        let res = resolve("jobs").unwrap();
        let runner = RecordingRunner::new();
        apply_mode(&res, Target::Grok, &PathBuf::from("/repo"), &runner).unwrap();
        assert!(runner.lines().iter().all(|l| l.starts_with("grok plugin ")));
        assert!(runner
            .lines()
            .contains(&"grok plugin enable job-hunter".to_string()));

        // Install calls (if any) must be for grok and only for plugins in the enabled set.
        // (Presence depends on whether the test machine's aip store has those plugin dirs.)
        let all_lines = runner.lines();
        let install_calls: Vec<_> = all_lines
            .iter()
            .filter(|l| l.contains("grok plugin install"))
            .collect();
        for call in &install_calls {
            assert!(call.contains("ai-architecture") || call.contains("job-hunter"));
        }
    }

    #[test]
    fn pi_target_installs_on_and_removes_off() {
        use std::fs;
        use tempfile::TempDir;

        let temp_home = TempDir::new().unwrap();
        let store_plugins = temp_home.path().join(".aip-cli").join("plugins");
        fs::create_dir_all(store_plugins.join("ai-architecture")).unwrap();
        fs::create_dir_all(store_plugins.join("software-engineer")).unwrap();

        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_home.path());

        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let actions = apply_mode(&res, Target::Pi, &PathBuf::from("/repo"), &runner).unwrap();

        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }

        let n = crate::modes::all_plugins().len();
        assert_eq!(actions.len(), n);
        assert_eq!(runner.lines().len(), n);

        let lines = runner.lines();
        assert!(lines.iter().all(|l| l.starts_with("pi ")));
        // Enabled plugins present in store → install.
        assert!(lines.iter().any(|l| {
            l.starts_with("pi install ") && l.contains("ai-architecture")
        }));
        assert!(lines.iter().any(|l| {
            l.starts_with("pi install ") && l.contains("software-engineer")
        }));
        // Off plugins → remove (even when not in store).
        assert!(lines.iter().any(|l| l.starts_with("pi remove ") && l.contains("flutter")));
        // Wanted enables that had dirs reported success.
        let on_ok: Vec<_> = actions.iter().filter(|a| a.enable && a.success).collect();
        assert_eq!(on_ok.len(), 2);
        // Wanted enables missing from store would fail — none missing here.
        assert!(!actions.iter().any(|a| a.enable && !a.success));
    }

    #[test]
    fn pi_enable_fails_when_plugin_missing_from_store() {
        use tempfile::TempDir;

        let temp_home = TempDir::new().unwrap();
        // Empty store — minimal's plugins are absent.
        std::fs::create_dir_all(temp_home.path().join(".aip-cli").join("plugins")).unwrap();
        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_home.path());

        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let actions = apply_mode(&res, Target::Pi, &PathBuf::from("/r"), &runner).unwrap();

        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }

        // Enables fail (no store dir); no install calls issued for them.
        assert_eq!(actions.iter().filter(|a| a.enable && !a.success).count(), 2);
        assert!(!runner.lines().iter().any(|l| l.starts_with("pi install ")));
        // Removes still issued for the rest of the catalog.
        assert!(runner.lines().iter().any(|l| l.starts_with("pi remove ")));
    }

    #[test]
    fn target_parse_and_key() {
        assert_eq!(Target::parse("claude"), Some(Target::ClaudeCode));
        assert_eq!(Target::parse("GROK"), Some(Target::Grok));
        assert_eq!(Target::parse("pi"), Some(Target::Pi));
        assert_eq!(Target::parse("PI"), Some(Target::Pi));
        assert_eq!(Target::parse("gemini"), None);
        assert_eq!(Target::ClaudeCode.key(), "claude");
        assert_eq!(Target::Pi.key(), "pi");
        assert_eq!(Target::Pi.program(), "pi");
    }

    #[test]
    fn available_filters_by_predicate() {
        // Only grok installed.
        let got = available(&ALL_TARGETS, |prog| prog == "grok");
        assert_eq!(got, vec![Target::Grok]);
        // Nothing installed.
        assert!(available(&ALL_TARGETS, |_| false).is_empty());
        // Everything installed.
        assert_eq!(available(&ALL_TARGETS, |_| true), ALL_TARGETS.to_vec());
    }

    #[test]
    fn apply_targets_hits_every_agent() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let reports = apply_targets(&res, &ALL_TARGETS, &PathBuf::from("/repo"), &runner).unwrap();
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].target, Target::ClaudeCode);
        assert_eq!(reports[1].target, Target::Grok);
        assert_eq!(reports[2].target, Target::Pi);

        // Compute expected pre-installs based on *actual* store state at test time
        // (makes test robust across machines with/without populated ~/.aip-cli/plugins).
        let store = store::plugins_dir();
        let (on, _) = res.partition();
        let grok_preinstalls = on.iter().filter(|name| store.join(name).is_dir()).count();
        // Pi install only for enabled plugins present in the store; remove for the rest.
        let pi_installs = on.iter().filter(|name| store.join(name).is_dir()).count();
        let pi_removes = crate::modes::all_plugins().len() - on.len();
        // When store lacks enabled plugins, pi issues no install (counts as failed enable).
        let pi_calls = pi_installs + pi_removes;

        // Claude: n. Grok: n + preinstalls. Pi: installs for present-on + removes for off.
        let n = crate::modes::all_plugins().len();
        assert_eq!(
            runner.lines().len(),
            n + n + grok_preinstalls + pi_calls
        );

        assert!(runner
            .lines()
            .contains(&"claude plugin enable ai-architecture".to_string()));
        assert!(runner
            .lines()
            .contains(&"grok plugin enable ai-architecture".to_string()));
        assert!(runner.lines().iter().any(|l| l.starts_with("pi ")));
    }

    #[test]
    fn apply_targets_empty_is_noop() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let reports = apply_targets(&res, &[], &PathBuf::from("/r"), &runner).unwrap();
        assert!(reports.is_empty());
        assert!(runner.lines().is_empty());
    }

    #[test]
    fn is_on_path_detects_absolute_and_bare_names() {
        // Iteration 3
        assert!(is_on_path("/bin/sh")); // absolute existing
        assert!(is_on_path("sh"));      // bare, should be on PATH on unix/mac
        assert!(!is_on_path("/this/does/not/exist/really123"));
        assert!(!is_on_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn preinstall_skips_when_target_not_grok() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        // Even if store has dirs, claude target must never emit grok install
        let _ = apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/r"), &runner);
        assert!(!runner.lines().iter().any(|l| l.contains("grok")));
    }

    #[test]
    fn tolerates_individual_failures() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::failing(|inv| inv.args.contains(&"disable".to_string()));
        // Should not error even though every disable "fails".
        let actions = apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/r"), &runner).unwrap();
        assert_eq!(actions.iter().filter(|a| a.enable).count(), 2);
    }

    #[test]
    fn grok_preinstall_only_for_plugins_present_in_store() {
        // TDD iteration 1: control the aip store via HOME to simulate partial ingest.
        // "minimal" enables ai-architecture + software-engineer.
        // Create dir only for one of them under a temp store.
        use std::fs;
        use tempfile::TempDir;

        let temp_home = TempDir::new().unwrap();
        let store_plugins = temp_home.path().join(".aip-cli").join("plugins");
        fs::create_dir_all(&store_plugins).unwrap();

        // Only "ai-architecture" present in this fake store (simulates partial ingest).
        let present = store_plugins.join("ai-architecture");
        fs::create_dir_all(&present).unwrap();

        let old_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", temp_home.path());

        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::new();
        let _ = apply_mode(&res, Target::Grok, &PathBuf::from("/repo"), &runner);

        // Restore
        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }

        let lines = runner.lines();

        // Exactly one pre-install (only for the dir that existed)
        let install_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("grok plugin install"))
            .collect();
        assert_eq!(install_lines.len(), 1);
        assert!(install_lines[0].contains("ai-architecture"));
        assert!(!install_lines.iter().any(|l| l.contains("software-engineer")));

        // Still performs enable/disable for *all* catalog plugins (pre-install is additive)
        let n = crate::modes::all_plugins().len();
        assert_eq!(lines.len(), n + 1); // +1 for the one install

        // The enable for the present one (and the other) must still be issued
        assert!(lines.iter().any(|l| l.contains("grok plugin enable ai-architecture")));
        assert!(lines.iter().any(|l| l.contains("grok plugin enable software-engineer")));

        // Verify that the install invocation used the caller's cwd (not the plugin dir).
        let calls = runner.calls();
        if let Some(install) = calls.iter().find(|c| c.args.iter().any(|a| a == "install")) {
            assert_eq!(install.cwd, PathBuf::from("/repo"));
        }
    }

    #[test]
    fn grok_preinstall_failure_is_ignored_and_enables_are_still_attempted() {
        // Iteration 2: simulate `grok plugin install` failing (bad manifest, permission,
        // grok not liking the dir, etc.). The apply must continue and still issue
        // the enable/disable actions.
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::failing(|inv| {
            inv.args.iter().any(|a| a == "install")
        });
        let actions = apply_mode(&res, Target::Grok, &PathBuf::from("/r"), &runner).unwrap();

        let n = crate::modes::all_plugins().len();
        assert_eq!(actions.len(), n);

        // Wanted enables were still attempted (the install failure was swallowed)
        assert_eq!(actions.iter().filter(|a| a.enable).count(), 2);

        // We did attempt the installs (they "failed" per the mock)
        assert!(runner
            .lines()
            .iter()
            .any(|l| l.contains("grok plugin install")));
    }
}
