//! Setup orchestration: optionally run each plugin's `prepare`, then `install`.
//!
//! Mirrors `run_all.py`:
//! - **prepare** — run `make prepare` for every plugin whose Makefile has a
//!   `prepare:` target; skip the rest. This is a *vendoring* build step: a
//!   plugin's `prepare` typically shells out to `../../scripts/vendor_shared.py`
//!   to bake shared reference files into the plugin. That path only resolves in
//!   the **source repo** (`<repo>/plugins/<name>/../../scripts`), so prepare runs
//!   only when setup operates against a source checkout — never against the
//!   `~/.aip-cli` store, whose copies are already self-contained. The `vendor`
//!   flag on [`run_setup`] carries that distinction.
//! - **install** — run `make link` when the plugin is already dev-linked into the
//!   Claude cache, otherwise `make setup`.

use crate::discovery::Plugin;
use crate::runner::{make, CommandRunner, Invocation};
use std::path::{Path, PathBuf};

/// Outcome of a single phase step for one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub plugin: String,
    pub phase: &'static str,
    /// `"ok"`, `"fail"`, or `"skip"`.
    pub status: &'static str,
    pub command: Option<String>,
}

/// The install target to use for a plugin given whether it is already linked.
pub fn install_target(linked: bool) -> &'static str {
    if linked {
        "link"
    } else {
        "setup"
    }
}

/// Path of the cache dir Claude dev-links a plugin into:
/// `~/.claude/plugins/cache/<name>/<name>/<version>`.
pub fn cache_link_path(home: &Path, plugin: &Plugin) -> PathBuf {
    home.join(".claude")
        .join("plugins")
        .join("cache")
        .join(&plugin.name)
        .join(&plugin.name)
        .join(&plugin.version)
}

/// Real link detection: the cache path exists and is a symlink.
pub fn is_linked(home: &Path, plugin: &Plugin) -> bool {
    let p = cache_link_path(home, plugin);
    std::fs::symlink_metadata(&p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Run the `prepare` phase across `plugins`. Plugins without a prepare target are
/// recorded as skipped and no command runs for them.
pub fn run_prepare<R: CommandRunner>(plugins: &[Plugin], runner: &R) -> std::io::Result<Vec<Step>> {
    let mut steps = Vec::new();
    for p in plugins {
        if !p.has_prepare {
            steps.push(Step {
                plugin: p.name.clone(),
                phase: "prepare",
                status: "skip",
                command: None,
            });
            continue;
        }
        let inv = make("prepare", &p.path);
        let out = runner.run(&inv)?;
        steps.push(Step {
            plugin: p.name.clone(),
            phase: "prepare",
            status: if out.success { "ok" } else { "fail" },
            command: Some(inv.display()),
        });
    }
    Ok(steps)
}

/// Run the `install` phase across `plugins`. `linked` decides `link` vs `setup`
/// per plugin (inject for testing; in production pass `|p| is_linked(home, p)`).
pub fn run_install<R, F>(plugins: &[Plugin], runner: &R, linked: F) -> std::io::Result<Vec<Step>>
where
    R: CommandRunner,
    F: Fn(&Plugin) -> bool,
{
    let mut steps = Vec::new();
    for p in plugins {
        let target = install_target(linked(p));
        let inv = Invocation::new("make", &[target], &p.path);
        let out = runner.run(&inv)?;
        steps.push(Step {
            plugin: p.name.clone(),
            phase: "install",
            status: if out.success { "ok" } else { "fail" },
            command: Some(inv.display()),
        });
    }
    Ok(steps)
}

/// Full setup: optionally prepare (vendor), then install. Returns all steps in
/// order.
///
/// `vendor` gates the prepare phase: pass `true` when `plugins` live in a source
/// repo (their `../../scripts` vendoring tooling resolves), `false` when they are
/// store copies that are already self-contained. When `false`, every plugin's
/// prepare step is recorded as skipped and no `make prepare` runs.
pub fn run_setup<R, F>(
    plugins: &[Plugin],
    runner: &R,
    linked: F,
    vendor: bool,
) -> std::io::Result<Vec<Step>>
where
    R: CommandRunner,
    F: Fn(&Plugin) -> bool,
{
    let mut steps = if vendor {
        run_prepare(plugins, runner)?
    } else {
        plugins
            .iter()
            .map(|p| Step {
                plugin: p.name.clone(),
                phase: "prepare",
                status: "skip",
                command: None,
            })
            .collect()
    };
    steps.extend(run_install(plugins, runner, linked)?);
    Ok(steps)
}

/// Outcome counts for host-side package installs (e.g. `pi install`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostInstallReport {
    /// Invocations issued (`ok + failed`).
    pub attempted: usize,
    pub ok: usize,
    /// `outcome.success == false` or `runner.run` returned `Err`.
    pub failed: usize,
    /// Canonicalize failed / path missing — no invocation.
    pub skipped: usize,
}

/// Sequential `pi install <canonical-abs>` for each plugin.
///
/// Caller gates PATH. No `Target`, no `-l`/`--trust`. Continues on install
/// failure. Does not call `is_on_path`.
pub fn install_pi_packages<R: CommandRunner>(
    plugins: &[Plugin],
    runner: &R,
) -> HostInstallReport {
    let mut report = HostInstallReport::default();
    for p in plugins {
        let abs = match std::fs::canonicalize(&p.path) {
            Ok(path) => path,
            Err(_) => {
                report.skipped += 1;
                continue;
            }
        };
        let abs_str = abs.to_string_lossy().into_owned();
        let inv = Invocation::new("pi", &["install", abs_str.as_str()], &abs);
        report.attempted += 1;
        match runner.run(&inv) {
            Ok(out) if out.success => report.ok += 1,
            Ok(_) | Err(_) => report.failed += 1,
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{CommandRunner, Invocation, Outcome, RecordingRunner};
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn plugin(name: &str, prepare: bool) -> Plugin {
        Plugin {
            dir_name: name.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            has_prepare: prepare,
            path: PathBuf::from(format!("/repo/plugins/{name}")),
        }
    }

    fn temp_plugin(name: &str, dir: &std::path::Path) -> Plugin {
        let path = dir.join(name);
        std::fs::create_dir_all(&path).unwrap();
        Plugin {
            dir_name: name.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            has_prepare: false,
            path,
        }
    }

    /// Returns `Err` for every inv — exercises the `run` error branch.
    struct ErrRunner {
        calls: Mutex<Vec<Invocation>>,
    }

    impl ErrRunner {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for ErrRunner {
        fn run(&self, inv: &Invocation) -> std::io::Result<Outcome> {
            self.calls.lock().unwrap().push(inv.clone());
            Err(std::io::Error::other("forced"))
        }
    }

    #[test]
    fn install_target_picks_link_or_setup() {
        assert_eq!(install_target(true), "link");
        assert_eq!(install_target(false), "setup");
    }

    #[test]
    fn cache_link_path_layout() {
        let p = plugin("product", false);
        let path = cache_link_path(Path::new("/home/dan"), &p);
        assert!(path.ends_with("product/product/1.0.0"));
        assert!(path.starts_with("/home/dan/.claude/plugins/cache"));
    }

    #[test]
    fn prepare_skips_plugins_without_target() {
        let plugins = vec![plugin("a", true), plugin("b", false)];
        let runner = RecordingRunner::new();
        let steps = run_prepare(&plugins, &runner).unwrap();
        assert_eq!(steps[0].status, "ok");
        assert_eq!(steps[1].status, "skip");
        // Only "a" actually ran make prepare.
        assert_eq!(runner.lines(), vec!["make prepare"]);
        assert_eq!(runner.calls()[0].cwd, PathBuf::from("/repo/plugins/a"));
    }

    #[test]
    fn install_uses_link_when_already_linked() {
        let plugins = vec![plugin("a", false), plugin("b", false)];
        let runner = RecordingRunner::new();
        // "a" linked, "b" not.
        let steps = run_install(&plugins, &runner, |p| p.name == "a").unwrap();
        assert_eq!(steps[0].command.as_deref(), Some("make link"));
        assert_eq!(steps[1].command.as_deref(), Some("make setup"));
    }

    #[test]
    fn full_setup_runs_prepare_then_install() {
        let plugins = vec![plugin("a", true)];
        let runner = RecordingRunner::new();
        let steps = run_setup(&plugins, &runner, |_| false, true).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].phase, "prepare");
        assert_eq!(steps[1].phase, "install");
        assert_eq!(runner.lines(), vec!["make prepare", "make setup"]);
    }

    #[test]
    fn setup_without_vendor_skips_prepare_entirely() {
        // Store copies are self-contained: prepare must never run, even for a
        // plugin whose Makefile has a prepare target (its ../../scripts vendoring
        // path would not resolve from the store).
        let plugins = vec![plugin("a", true)];
        let runner = RecordingRunner::new();
        let steps = run_setup(&plugins, &runner, |_| false, false).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].phase, "prepare");
        assert_eq!(steps[0].status, "skip");
        assert_eq!(steps[0].command, None);
        assert_eq!(steps[1].phase, "install");
        // Only the install target ran; no `make prepare`.
        assert_eq!(runner.lines(), vec!["make setup"]);
    }

    #[test]
    fn install_marks_failure() {
        let plugins = vec![plugin("a", false)];
        let runner = RecordingRunner::failing(|_| true);
        let steps = run_install(&plugins, &runner, |_| false).unwrap();
        assert_eq!(steps[0].status, "fail");
    }

    #[test]
    fn pi_install_emits_abs_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plugins = vec![
            temp_plugin("a", tmp.path()),
            temp_plugin("b", tmp.path()),
        ];
        let runner = RecordingRunner::new();
        let report = install_pi_packages(&plugins, &runner);
        assert_eq!(report.ok, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.attempted, 2);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        for (i, p) in plugins.iter().enumerate() {
            let abs = std::fs::canonicalize(&p.path).unwrap();
            assert_eq!(calls[i].program, "pi");
            assert_eq!(calls[i].args, vec!["install".to_string(), abs.to_string_lossy().into_owned()]);
            assert!(Path::new(&calls[i].args[1]).is_absolute());
            assert_eq!(calls[i].cwd, abs);
        }
    }

    #[test]
    fn pi_install_relative_path_becomes_abs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let name = "relplug";
        let abs_dir = tmp.path().join(name);
        std::fs::create_dir_all(&abs_dir).unwrap();
        // Non-canonical absolute path with `.` / `..` components → canonicalize to abs.
        let mut p = temp_plugin(name, tmp.path());
        p.path = abs_dir.join(".").join("..").join(name);
        let runner = RecordingRunner::new();
        let report = install_pi_packages(&[p], &runner);
        assert_eq!(report.ok, 1);
        assert_eq!(report.skipped, 0);
        let arg = &runner.calls()[0].args[1];
        assert!(Path::new(arg).is_absolute());
        assert!(!arg.contains("/./") && !arg.contains("/../"));
        assert_eq!(
            Path::new(arg),
            std::fs::canonicalize(tmp.path().join(name)).unwrap()
        );
    }

    #[test]
    fn pi_install_skips_missing_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let good = temp_plugin("good", tmp.path());
        let bad = Plugin {
            dir_name: "missing".into(),
            name: "missing".into(),
            version: "1.0.0".into(),
            has_prepare: false,
            path: tmp.path().join("does-not-exist"),
        };
        let runner = RecordingRunner::new();
        let report = install_pi_packages(&[good.clone(), bad], &runner);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.ok, 1);
        assert_eq!(report.attempted, 1);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(
            runner.calls()[0].args[1],
            std::fs::canonicalize(&good.path)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        );
    }

    #[test]
    fn pi_install_counts_runner_failures() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plugins = vec![
            temp_plugin("a", tmp.path()),
            temp_plugin("b", tmp.path()),
        ];
        let runner = RecordingRunner::failing(|_| true);
        let report = install_pi_packages(&plugins, &runner);
        assert_eq!(report.failed, 2);
        assert_eq!(report.attempted, 2);
        assert_eq!(report.ok, 0);
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn pi_install_counts_run_err() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plugins = vec![temp_plugin("a", tmp.path())];
        let runner = ErrRunner::new();
        let report = install_pi_packages(&plugins, &runner);
        assert_eq!(report.failed, 1);
        assert_eq!(report.attempted, 1);
        assert_eq!(report.ok, 0);
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn pi_install_empty() {
        let runner = RecordingRunner::new();
        let report = install_pi_packages(&[], &runner);
        assert_eq!(report, HostInstallReport::default());
        assert!(runner.calls().is_empty());
    }
}
