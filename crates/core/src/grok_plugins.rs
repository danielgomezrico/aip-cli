//! Our own abstraction over the Grok plugin CLI, so `aip-cli setup` can make
//! grok serve the store's latest code only — mirroring [`crate::claude_plugins`].
//!
//! Grok does not replace a plugin on re-install: `grok plugin install <path>
//! --trust` hard-fails with `repo '<name>-<hash>' already installed`. Worse,
//! successive installs from different source paths leave DUPLICATE registrations
//! for one plugin name (e.g. a store copy plus a drifted `~/projects/.../plugins/<name>`
//! copy). To guarantee replacement we uninstall every registration of a name,
//! then install fresh from the store.
//!
//! Contract, verified against the live grok CLI:
//! - `grok plugin list` prints one registration per line as
//!   `<repo-id>: <name> [local: <path>]` (the `[local: …]` suffix is absent for
//!   non-local sources).
//! - `grok plugin uninstall <name> --confirm` accepts **only** the plugin name
//!   (repo ids are rejected), removes exactly ONE registration per call, and
//!   with duplicates present does not guarantee which one goes first.
//! - `grok plugin install <path> --trust` registers a local directory.
//!
//! Mutations (`uninstall`/`install`) flow through a [`CommandRunner`] so their
//! exact command shapes are unit-testable. Reading the list is a *state read*
//! (like [`crate::claude_plugins`] reading `known_marketplaces.json` from disk):
//! [`capture_list`] runs the real `grok plugin list` and returns its stdout,
//! while the orchestrator takes the list text via an injected reader so tests
//! can script it.

use crate::runner::{CommandRunner, Invocation};
use std::path::Path;
use std::process::{Command, Stdio};

/// `grok plugin list` — enumerate registrations. Run from `cwd`.
pub fn list_invocation(cwd: &Path) -> Invocation {
    Invocation::new("grok", &["plugin", "list"], cwd)
}

/// `grok plugin uninstall <name> --confirm` — remove ONE registration of `name`.
pub fn uninstall_invocation(name: &str, cwd: &Path) -> Invocation {
    Invocation::new("grok", &["plugin", "uninstall", name, "--confirm"], cwd)
}

/// `grok plugin install <path> --trust` — register a local plugin directory.
/// `--trust` is required for directory installs: grok otherwise refuses the
/// interactive-confirmation prompt in this non-tty context.
pub fn install_invocation(path: &Path, cwd: &Path) -> Invocation {
    let p = path.to_string_lossy().into_owned();
    Invocation::new("grok", &["plugin", "install", &p, "--trust"], cwd)
}

/// One parsed `grok plugin list` registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokRegistration {
    /// Repo id `<name>-<hash>` — the left field. Not accepted by `uninstall`.
    pub repo_id: String,
    /// Plugin name — the field `uninstall`/`update` accept.
    pub name: String,
    /// Local source path from a `[local: <path>]` suffix, when present.
    pub local_path: Option<String>,
}

/// Parse `grok plugin list` output into registrations, skipping malformed lines.
pub fn parse_list(text: &str) -> Vec<GrokRegistration> {
    text.lines().filter_map(parse_line).collect()
}

/// Parse one list line `<repo-id>: <name> [local: <path>]`. Returns `None` for
/// blank or malformed lines (missing the `<repo-id>: ` separator or an empty
/// name), tolerating a missing `[local: …]` suffix.
fn parse_line(line: &str) -> Option<GrokRegistration> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let (repo_id, rest) = line.split_once(": ")?;
    let repo_id = repo_id.trim();
    let rest = rest.trim();
    if repo_id.is_empty() || rest.is_empty() {
        return None;
    }
    let (name, local_path) = match rest.split_once(" [") {
        Some((n, bracket)) => {
            let path = bracket
                .strip_suffix(']')
                .and_then(|b| b.strip_prefix("local: "))
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            (n.trim(), path)
        }
        None => (rest, None),
    };
    if name.is_empty() {
        return None;
    }
    Some(GrokRegistration {
        repo_id: repo_id.to_string(),
        name: name.to_string(),
        local_path,
    })
}

/// Count registrations of `name` in list `text`.
fn count_registrations(text: &str, name: &str) -> usize {
    parse_list(text)
        .into_iter()
        .filter(|e| e.name == name)
        .count()
}

/// Run the real `grok plugin list` and return its stdout (empty on any failure).
/// This is the state read the orchestrator's `read_list` reader uses in
/// production; it lives here so all grok CLI knowledge stays in this module.
pub fn capture_list(cwd: &Path) -> String {
    let inv = list_invocation(cwd);
    Command::new(&inv.program)
        .args(&inv.args)
        .current_dir(&inv.cwd)
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Replace every grok registration of `name` with a fresh install from
/// `install_path`, so grok serves the store's latest code only.
///
/// Reads the list via `read_list`; while any registration of `name` remains,
/// issues `uninstall <name> --confirm` and re-reads — bounded by the initial
/// match count plus a small safety margin so a failing uninstall (list never
/// shrinks) can't loop forever. Then installs `install_path`. Every host call is
/// best-effort: a plugin grok never knew, or a missing `grok`, must not abort
/// setup.
pub fn replace_plugin<R: CommandRunner + ?Sized>(
    runner: &R,
    name: &str,
    install_path: &Path,
    cwd: &Path,
    mut read_list: impl FnMut() -> String,
) {
    let mut remaining = count_registrations(&read_list(), name);
    // Safety margin over the initial count: bounds the loop when uninstall
    // fails and the list never shrinks (grok removes exactly one per call, so
    // `remaining` calls suffice when they succeed).
    let cap = remaining + 2;
    let mut attempts = 0;
    while remaining > 0 && attempts < cap {
        let _ = runner.run(&uninstall_invocation(name, cwd));
        attempts += 1;
        remaining = count_registrations(&read_list(), name);
    }
    let _ = runner.run(&install_invocation(install_path, cwd));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingRunner;
    use std::cell::RefCell;
    use std::rc::Rc;

    const ENTRY: &str = "frontend-a1b2c3: frontend [local: /Users/x/.aip-cli/plugins/frontend]";
    const DRIFTED: &str =
        "frontend-d4e5f6: frontend [local: /Users/x/projects/claude/plugins/frontend]";

    /// A `read_list` reader that yields `outputs` in order, then repeats the last
    /// one forever (so an over-eager loop keeps seeing a non-shrinking list).
    fn scripted(outputs: Vec<&str>) -> impl FnMut() -> String {
        let outputs: Vec<String> = outputs.into_iter().map(String::from).collect();
        let i = Rc::new(RefCell::new(0usize));
        move || {
            let mut idx = i.borrow_mut();
            let s = outputs
                .get(*idx)
                .or_else(|| outputs.last())
                .cloned()
                .unwrap_or_default();
            *idx += 1;
            s
        }
    }

    #[test]
    fn invocation_shapes() {
        assert_eq!(
            list_invocation(Path::new("/c")).display(),
            "grok plugin list"
        );
        assert_eq!(
            uninstall_invocation("frontend", Path::new("/c")).display(),
            "grok plugin uninstall frontend --confirm"
        );
        assert_eq!(
            install_invocation(Path::new("/s/frontend"), Path::new("/c")).display(),
            "grok plugin install /s/frontend --trust"
        );
    }

    #[test]
    fn parse_list_reads_real_line_shape() {
        let got = parse_list(ENTRY);
        assert_eq!(
            got,
            vec![GrokRegistration {
                repo_id: "frontend-a1b2c3".to_string(),
                name: "frontend".to_string(),
                local_path: Some("/Users/x/.aip-cli/plugins/frontend".to_string()),
            }]
        );
    }

    #[test]
    fn parse_list_captures_duplicate_names_with_distinct_paths() {
        let text = format!("{ENTRY}\n{DRIFTED}");
        let got = parse_list(&text);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|e| e.name == "frontend"));
        // Same name, different local source paths (store vs drifted).
        assert_ne!(got[0].local_path, got[1].local_path);
        assert_eq!(count_registrations(&text, "frontend"), 2);
    }

    #[test]
    fn parse_list_tolerates_malformed_and_bracketless_lines() {
        let text = "\
\n\
   \n\
no-separator-here\n\
: missing-repo-id\n\
software-engineer-99: software-engineer\n\
job-hunter-77: job-hunter [local: /Users/x/.aip-cli/plugins/job-hunter]\n";
        let got = parse_list(text);
        // Blank / separator-less / empty-repo-id lines are skipped.
        assert_eq!(got.len(), 2);
        // A line with no `[local: …]` parses with local_path = None.
        assert_eq!(got[0].name, "software-engineer");
        assert_eq!(got[0].local_path, None);
        assert_eq!(got[1].name, "job-hunter");
        assert_eq!(
            got[1].local_path.as_deref(),
            Some("/Users/x/.aip-cli/plugins/job-hunter")
        );
    }

    #[test]
    fn replace_fresh_install_when_not_registered() {
        let runner = RecordingRunner::new();
        replace_plugin(
            &runner,
            "frontend",
            Path::new("/store/frontend"),
            Path::new("/c"),
            scripted(vec![""]),
        );
        assert_eq!(
            runner.lines(),
            vec!["grok plugin install /store/frontend --trust"]
        );
    }

    #[test]
    fn replace_single_store_backed_reinstall() {
        let runner = RecordingRunner::new();
        // First read shows one registration; after the uninstall, the list is empty.
        replace_plugin(
            &runner,
            "frontend",
            Path::new("/store/frontend"),
            Path::new("/c"),
            scripted(vec![ENTRY, ""]),
        );
        assert_eq!(
            runner.lines(),
            vec![
                "grok plugin uninstall frontend --confirm",
                "grok plugin install /store/frontend --trust",
            ]
        );
    }

    #[test]
    fn replace_removes_both_duplicates_then_installs() {
        let runner = RecordingRunner::new();
        let both = format!("{ENTRY}\n{DRIFTED}");
        // Two registrations → after first uninstall one remains → after second none.
        replace_plugin(
            &runner,
            "frontend",
            Path::new("/store/frontend"),
            Path::new("/c"),
            scripted(vec![&both, ENTRY, ""]),
        );
        assert_eq!(
            runner.lines(),
            vec![
                "grok plugin uninstall frontend --confirm",
                "grok plugin uninstall frontend --confirm",
                "grok plugin install /store/frontend --trust",
            ]
        );
    }

    #[test]
    fn replace_terminates_and_installs_when_uninstall_never_clears() {
        // Uninstall "fails" (list never shrinks): the loop must be bounded and the
        // install must still be attempted. cap = initial(1) + margin(2) = 3.
        let runner = RecordingRunner::failing(|inv| inv.args.iter().any(|a| a == "uninstall"));
        replace_plugin(
            &runner,
            "frontend",
            Path::new("/store/frontend"),
            Path::new("/c"),
            scripted(vec![ENTRY]), // always one registration
        );
        let lines = runner.lines();
        assert_eq!(
            lines.iter().filter(|l| l.contains("uninstall")).count(),
            3,
            "loop must be bounded by initial-count + margin"
        );
        assert_eq!(
            lines.last().map(String::as_str),
            Some("grok plugin install /store/frontend --trust"),
            "install must still be attempted after a stuck uninstall loop"
        );
    }

    #[test]
    fn replace_ignores_other_plugin_registrations() {
        let runner = RecordingRunner::new();
        // The list holds a different plugin; `frontend` is absent → no uninstalls.
        let other = "software-engineer-11: software-engineer [local: /s/software-engineer]";
        replace_plugin(
            &runner,
            "frontend",
            Path::new("/store/frontend"),
            Path::new("/c"),
            scripted(vec![other]),
        );
        assert_eq!(
            runner.lines(),
            vec!["grok plugin install /store/frontend --trust"]
        );
    }
}
