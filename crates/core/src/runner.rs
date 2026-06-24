//! Command execution abstraction.
//!
//! Every external side effect (`make`, `claude plugin enable`, ...) goes through
//! [`CommandRunner`]. Production code uses [`SystemRunner`]; tests use
//! [`RecordingRunner`] to assert on the exact commands that *would* run without
//! touching the system.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// Runs commands for real via [`std::process::Command`], silencing the child's
/// stdout/stderr so each `plugin enable/disable` call doesn't spam the terminal
/// with its own per-plugin chatter — the caller renders one grouped summary
/// instead.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
        let status = Command::new(&inv.program)
            .args(&inv.args)
            .current_dir(&inv.cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(Outcome {
            success: status.success(),
            code: status.code(),
        })
    }
}

/// Test double: records every invocation and returns a configurable outcome.
///
/// By default every command "succeeds". Use [`RecordingRunner::failing`] to make
/// commands matching a predicate fail, exercising error paths.
pub struct RecordingRunner {
    calls: RefCell<Vec<Invocation>>,
    #[allow(clippy::type_complexity)]
    fail_when: Option<Box<dyn Fn(&Invocation) -> bool>>,
}

impl Default for RecordingRunner {
    fn default() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail_when: None,
        }
    }
}

impl RecordingRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner where commands matching `predicate` return a failing outcome.
    pub fn failing(predicate: impl Fn(&Invocation) -> bool + 'static) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail_when: Some(Box::new(predicate)),
        }
    }

    /// All recorded invocations, in order.
    pub fn calls(&self) -> Vec<Invocation> {
        self.calls.borrow().clone()
    }

    /// Rendered command lines, in order — convenient for assertions.
    pub fn lines(&self) -> Vec<String> {
        self.calls
            .borrow()
            .iter()
            .map(Invocation::display)
            .collect()
    }

    /// Working directories touched, deduplicated in first-seen order.
    pub fn cwds(&self) -> Vec<PathBuf> {
        let mut seen: Vec<PathBuf> = Vec::new();
        for c in self.calls.borrow().iter() {
            if !seen.contains(&c.cwd) {
                seen.push(c.cwd.clone());
            }
        }
        seen
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
        self.calls.borrow_mut().push(inv.clone());
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
