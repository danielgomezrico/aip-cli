//! Applying a resolved mode by enabling/disabling plugins via the host CLI.

use crate::modes::Resolution;
use crate::runner::{CommandRunner, Invocation};
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
/// for every plugin in canonical order. Failures of individual enable/disable
/// calls are tolerated (matching the original `|| true`); the returned vec lists
/// the actions taken in order.
pub fn apply_mode<R: CommandRunner>(
    resolution: &Resolution,
    target: Target,
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<Action>> {
    let (on, _off) = resolution.partition();
    let mut actions = Vec::new();
    for &plugin in crate::modes::ALL_PLUGINS {
        let enable = on.contains(&plugin);
        let verb = if enable { "enable" } else { "disable" };
        let inv = Invocation::new(target.program(), &["plugin", verb, plugin], cwd);
        // Tolerate failures, like the Makefile's `|| true`.
        let _ = runner.run(&inv)?;
        actions.push(Action { plugin, enable });
    }
    Ok(actions)
}

/// The result of applying a mode to one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetReport {
    pub target: Target,
    pub actions: Vec<Action>,
}

/// Apply `resolution` to every agent in `targets`, in order. Returns one report
/// per agent. An empty `targets` is a no-op (caller should warn).
pub fn apply_targets<R: CommandRunner>(
    resolution: &Resolution,
    targets: &[Target],
    cwd: &Path,
    runner: &R,
) -> std::io::Result<Vec<TargetReport>> {
    let mut reports = Vec::new();
    for &target in targets {
        let actions = apply_mode(resolution, target, cwd, runner)?;
        reports.push(TargetReport { target, actions });
    }
    Ok(reports)
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
        assert_eq!(runner.lines().len(), crate::modes::ALL_PLUGINS.len());
        assert_eq!(actions.len(), crate::modes::ALL_PLUGINS.len());

        // ai-architecture + software-engineer enabled, rest disabled.
        assert_eq!(runner.lines()[0], "claude plugin enable ai-architecture");
        assert_eq!(runner.lines()[1], "claude plugin enable software-engineer");
        assert_eq!(runner.lines()[2], "claude plugin disable flutter-pivara");
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
        let n = crate::modes::ALL_PLUGINS.len();
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
