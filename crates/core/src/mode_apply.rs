//! Applying a resolved mode by enabling/disabling plugins via the host CLI.

use crate::modes::Resolution;
use crate::runner::{CommandRunner, Invocation};
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
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claudecode" => Some(Target::ClaudeCode),
            "grok" => Some(Target::Grok),
            _ => None,
        }
    }

    /// Human label used in logs (matches the original `mode` / `mode-grok`).
    pub fn label(&self) -> &'static str {
        match self {
            Target::ClaudeCode => "mode",
            Target::Grok => "mode-grok",
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

/// One enable/disable decision for a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub plugin: &'static str,
    pub enable: bool,
}

/// Apply `resolution` against `target`, running `<prog> plugin enable|disable`
/// for every plugin in parallel. Failures of individual enable/disable
/// calls are tolerated (matching the original `|| true`); the returned vec lists
/// the actions taken in canonical order.
pub fn apply_mode<R: CommandRunner + Send + Sync + ?Sized>(
    resolution: &Resolution,
    target: Target,
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<Action>> {
    let (on, _off) = resolution.partition();
    let cwd_owned = cwd.to_path_buf();

    let actions: std::io::Result<Vec<_>> = crate::modes::all_plugins()
        .par_iter()
        .map(|&plugin| {
            let enable = on.contains(&plugin);
            let verb = if enable { "enable" } else { "disable" };
            let inv = Invocation::new(target.program(), &["plugin", verb, plugin], &cwd_owned);
            let _ = runner.run(&inv)?;
            Ok(Action { plugin, enable })
        })
        .collect();

    let mut result = actions?;
    // Restore canonical order.
    result.sort_by_key(|a| crate::modes::all_plugins().iter().position(|&p| p == a.plugin).unwrap_or(0));
    Ok(result)
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
        assert!(lines.iter().any(|l| l == "claude plugin enable ai-architecture"));
        assert!(lines.iter().any(|l| l == "claude plugin enable software-engineer"));
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
        // Each agent got one call per plugin.
        let n = crate::modes::all_plugins().len();
        assert_eq!(runner.lines().len(), 2 * n);
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
    fn tolerates_individual_failures() {
        let res = resolve("minimal").unwrap();
        let runner = RecordingRunner::failing(|inv| inv.args.contains(&"disable".to_string()));
        // Should not error even though every disable "fails".
        let actions = apply_mode(&res, Target::ClaudeCode, &PathBuf::from("/r"), &runner).unwrap();
        assert_eq!(actions.iter().filter(|a| a.enable).count(), 2);
    }
}
