//! Command execution abstraction.
//!
//! Every external side effect (`make`, `claude plugin enable`, ...) goes through
//! [`CommandRunner`]. Production code uses [`SystemRunner`]; tests use
//! [`RecordingRunner`] to assert on the exact commands that *would* run without
//! touching the system.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

/// A single command invocation: program, arguments, and working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl Invocation {
    pub fn new(program: impl Into<String>, args: &[&str], cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: cwd.into(),
        }
    }

    /// Render as a shell-ish line for logs and assertions, e.g. `make prepare`.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// Result of running a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    pub code: Option<i32>,
}

impl Outcome {
    pub fn ok() -> Self {
        Self {
            success: true,
            code: Some(0),
        }
    }
}

/// Abstraction over running external commands.
pub trait CommandRunner {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome>;
}

// Implement for Arc<R> to enable parallel execution.
impl<R: CommandRunner + ?Sized> CommandRunner for std::sync::Arc<R> {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
        (**self).run(inv)
    }
}

/// Runs commands for real via [`std::process::Command`], silencing the child's
/// stdout/stderr so each `plugin enable/disable` call doesn't spam the terminal
/// with its own per-plugin chatter — the caller renders one grouped summary
/// instead.
///
/// stdin is also detached (`/dev/null`): these are non-interactive, programmatic
/// invocations (`make`, `claude/grok plugin enable …`). Inheriting the terminal
/// would let a child see an interactive tty (`isatty` → true) and switch it to
/// raw/no-echo mode. Detaching stdin avoids a child that exits without restoring
/// termios leaving the user's shell with echo off (typed keys invisible until
/// Enter). A null stdin makes every child read EOF and never touch the
/// controlling terminal.
///
/// When `verbose` is set, the child's stdout/stderr is *captured* (still not
/// inherited, preserving the termios safety above) and echoed to our own stderr
/// in one block per command, along with the rendered command line and exit code.
/// This is how the user sees that e.g. `claude plugin enable flutter` actually
/// failed ("no such plugin") even though the per-plugin chatter is normally
/// hidden behind the grouped summary.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner {
    pub verbose: bool,
}

impl SystemRunner {
    /// A runner that silences child output (the default, production behaviour).
    pub fn new() -> Self {
        Self { verbose: false }
    }

    /// A runner that captures and echoes each command's output and exit code to
    /// stderr — used by `--verbose` to expose silently-tolerated failures.
    pub fn verbose() -> Self {
        Self { verbose: true }
    }
}

/// Decide whether a `plugin enable/disable` call left the plugin in the desired
/// state. The host CLIs exit non-zero for the common *no-op* case ("Plugin X is
/// already enabled" / "already disabled") — that's success, not failure. Any
/// other non-zero exit (e.g. "not found in any editable settings scope" after a
/// rename, or a broken config) is a genuine failure. A zero exit is always
/// success. This is why we must read stderr rather than trust the exit code
/// alone — and why the original code ignored exit codes entirely.
pub fn outcome_is_success(raw_success: bool, stderr: &str) -> bool {
    if raw_success {
        return true;
    }
    let s = stderr.to_ascii_lowercase();
    s.contains("already enabled") || s.contains("already disabled")
}

impl CommandRunner for SystemRunner {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
        // Always capture (never inherit) the child's stdout/stderr: we need
        // stderr to classify "already enabled" no-ops as success, and capturing
        // keeps the termios safety described above (stdin stays null, the child
        // never touches the controlling tty). Output is echoed only in verbose.
        let output = Command::new(&inv.program)
            .args(&inv.args)
            .current_dir(&inv.cwd)
            .stdin(Stdio::null())
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let success = outcome_is_success(output.status.success(), &stderr);
        let code = output.status.code();

        if self.verbose {
            // Build one block and write it in a single call so parallel runs
            // don't interleave line-by-line.
            let mut block = String::new();
            let marker = if success { "✓" } else { "✗" };
            block.push_str(&format!(
                "  [{marker}] {} (exit {})\n",
                inv.display(),
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            ));
            for (label, bytes) in [("out", &output.stdout), ("err", &output.stderr)] {
                let text = String::from_utf8_lossy(bytes);
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    block.push_str(&format!("      {label}| {line}\n"));
                }
            }
            eprint!("{block}");
        }
        Ok(Outcome { success, code })
    }
}

/// Test double: records every invocation and returns a configurable outcome.
///
/// By default every command "succeeds". Use [`RecordingRunner::failing`] to make
/// commands matching a predicate fail, exercising error paths.
pub struct RecordingRunner {
    calls: Mutex<Vec<Invocation>>,
    #[allow(clippy::type_complexity)]
    fail_when: Option<Box<dyn Fn(&Invocation) -> bool + Send + Sync>>,
}

impl Default for RecordingRunner {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_when: None,
        }
    }
}

impl RecordingRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner where commands matching `predicate` return a failing outcome.
    pub fn failing(predicate: impl Fn(&Invocation) -> bool + Send + Sync + 'static) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_when: Some(Box::new(predicate)),
        }
    }

    /// All recorded invocations, in order.
    pub fn calls(&self) -> Vec<Invocation> {
        self.calls.lock().unwrap().clone()
    }

    /// Rendered command lines, in order — convenient for assertions.
    pub fn lines(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(Invocation::display)
            .collect()
    }

    /// Working directories touched, deduplicated in first-seen order.
    pub fn cwds(&self) -> Vec<PathBuf> {
        let mut seen: Vec<PathBuf> = Vec::new();
        for c in self.calls.lock().unwrap().iter() {
            if !seen.contains(&c.cwd) {
                seen.push(c.cwd.clone());
            }
        }
        seen
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
        self.calls.lock().unwrap().push(inv.clone());
        let fail = self.fail_when.as_ref().map(|f| f(inv)).unwrap_or(false);
        Ok(Outcome {
            success: !fail,
            code: Some(if fail { 1 } else { 0 }),
        })
    }
}

/// Convenience: build a `make <target>` invocation in `dir`.
pub fn make(target: &str, dir: &Path) -> Invocation {
    Invocation::new("make", &[target], dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn outcome_zero_exit_is_success_regardless_of_stderr() {
        assert!(outcome_is_success(true, ""));
        assert!(outcome_is_success(true, "some warning noise"));
    }

    #[test]
    fn outcome_already_in_state_is_success() {
        assert!(outcome_is_success(
            false,
            r#"Plugin "x" is already enabled"#
        ));
        assert!(outcome_is_success(false, "Plugin already disabled"));
        // Case-insensitive.
        assert!(outcome_is_success(false, "ALREADY ENABLED"));
    }

    #[test]
    fn outcome_unknown_plugin_is_failure() {
        // The renamed-plugin case the user hit.
        assert!(!outcome_is_success(
            false,
            r#"Plugin "flutter" not found in any editable settings scope. Use plugin@marketplace format."#
        ));
        // Broken config is also a real failure.
        assert!(!outcome_is_success(false, "TOML parse error at line 18"));
        assert!(!outcome_is_success(false, ""));
    }

    #[test]
    fn invocation_display_joins_args() {
        let inv = Invocation::new("make", &["prepare"], "/x");
        assert_eq!(inv.display(), "make prepare");
        let bare = Invocation::new("ls", &[], "/x");
        assert_eq!(bare.display(), "ls");
    }

    #[test]
    fn recording_runner_records_in_order() {
        let r = RecordingRunner::new();
        r.run(&Invocation::new("make", &["prepare"], "/a")).unwrap();
        r.run(&Invocation::new("make", &["setup"], "/b")).unwrap();
        assert_eq!(r.lines(), vec!["make prepare", "make setup"]);
        assert_eq!(r.cwds(), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn recording_runner_default_succeeds() {
        let r = RecordingRunner::new();
        let out = r.run(&Invocation::new("make", &["x"], "/a")).unwrap();
        assert!(out.success);
    }

    #[test]
    fn recording_runner_failing_predicate() {
        let r = RecordingRunner::failing(|inv| inv.args.iter().any(|a| a == "boom"));
        assert!(
            r.run(&Invocation::new("make", &["ok"], "/a"))
                .unwrap()
                .success
        );
        assert!(
            !r.run(&Invocation::new("make", &["boom"], "/a"))
                .unwrap()
                .success
        );
    }

    #[test]
    fn cwds_dedup_first_seen() {
        let r = RecordingRunner::new();
        r.run(&Invocation::new("make", &["a"], "/a")).unwrap();
        r.run(&Invocation::new("make", &["b"], "/a")).unwrap();
        r.run(&Invocation::new("make", &["c"], "/b")).unwrap();
        assert_eq!(r.cwds(), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
