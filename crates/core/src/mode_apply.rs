//! Applying a resolved mode by enabling/disabling plugins via the host CLI.

use crate::arg_enum::ArgEnum;
use crate::modes::Resolution;
use crate::runner::{CommandRunner, Invocation};
use crate::store;
use rayon::prelude::*;
use std::path::Path;

/// Which host CLI receives the `plugin enable/disable` calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    ClaudeCode,
    Grok,
}

/// Every AI agent the CLI can drive, i.e. every host CLI that exposes a
/// `plugin enable/disable` interface. A mode is applied to all of these that are
/// actually installed, so the user never has to pick one.
pub const ALL_TARGETS: [Target; 2] = [Target::ClaudeCode, Target::Grok];

impl Target {
    /// The program name to invoke.
    pub fn program(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude",
            Target::Grok => "grok",
        }
    }

    /// Short stable key used on the CLI and in markers.
    pub fn key(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude",
            Target::Grok => "grok",
        }
    }

    /// Parse a target from its key (`claude` / `grok`).
    pub fn parse(s: &str) -> Option<Target> {
        <Target as ArgEnum>::parse(s)
    }

    /// Human label used in logs (matches the original `mode` / `mode-grok`).
    pub fn label(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "mode",
            Target::Grok => "mode-grok",
        }
    }
}

impl ArgEnum for Target {
    fn from_normalized(token: &str) -> Option<Target> {
        match token {
            "claude" | "claude-code" | "claudecode" => Some(Target::ClaudeCode),
            "grok" => Some(Target::Grok),
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
/// it. `success == false` means the `plugin enable/disable` call exited non-zero
/// (e.g. the agent doesn't know that plugin name) — the run continues, but the
/// caller can surface it instead of pretending the plugin is now enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub plugin: String,
    pub enable: bool,
    pub success: bool,
}

/// Apply `resolution` against `target`, running `<prog> plugin enable|disable`
/// for every plugin **sequentially** in canonical order. The calls must NOT run
/// in parallel: each `claude/grok plugin enable|disable` does a read-modify-write
/// of the same host config file (`~/.grok/config.toml`, claude's settings), and
/// concurrent invocations clobber each other — a torn write leaves trailing bytes
/// from a longer previous version, corrupting the TOML/JSON so every later command
/// fails to parse (observed: grok config truncated mid-array). Different *targets*
/// touch different files, so cross-target parallelism (in [`apply_targets`]) stays
/// safe; only within a target must the writes serialize.
///
/// Failures of individual enable/disable calls are tolerated (matching the
/// original `|| true`); the returned vec lists the actions in canonical order.
pub fn apply_mode<R: CommandRunner + Send + Sync + ?Sized>(
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
    fn target_parse_and_key() {
        assert_eq!(Target::parse("claude"), Some(Target::ClaudeCode));
        assert_eq!(Target::parse("GROK"), Some(Target::Grok));
        assert_eq!(Target::parse("gemini"), None);
        assert_eq!(Target::ClaudeCode.key(), "claude");
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
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].target, Target::ClaudeCode);
        assert_eq!(reports[1].target, Target::Grok);

        // Compute expected pre-installs based on *actual* store state at test time
        // (makes test robust across machines with/without populated ~/.aip-cli/plugins).
        let store = store::plugins_dir();
        let grok_preinstalls = res
            .enabled
            .iter()
            .filter(|name| store.join(name).is_dir())
            .count();

        // Claude: n calls. Grok: n calls + preinstalls for this target.
        let n = crate::modes::all_plugins().len();
        assert_eq!(runner.lines().len(), 2 * n + grok_preinstalls);

        assert!(runner
            .lines()
            .contains(&"claude plugin enable ai-architecture".to_string()));
        assert!(runner
            .lines()
            .contains(&"grok plugin enable ai-architecture".to_string()));
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
