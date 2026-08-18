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
use crate::removed::is_setup_blocked;
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

/// Run the `prepare` phase across `plugins`. Plugins without a prepare target
/// or with a `.aip-removed` marker (here or in `store_root`) are recorded as
/// skipped and no command runs for them.
pub fn run_prepare<R: CommandRunner>(
    plugins: &[Plugin],
    runner: &R,
    store_root: &Path,
) -> std::io::Result<Vec<Step>> {
    let mut steps = Vec::new();
    for p in plugins {
        if is_setup_blocked(&p.path, &store_root.join(&p.dir_name)) || !p.has_prepare {
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
pub fn run_install<R, F>(
    plugins: &[Plugin],
    runner: &R,
    linked: F,
    store_root: &Path,
) -> std::io::Result<Vec<Step>>
where
    R: CommandRunner,
    F: Fn(&Plugin) -> bool,
{
    let mut steps = Vec::new();
    for p in plugins {
        if is_setup_blocked(&p.path, &store_root.join(&p.dir_name)) {
            steps.push(Step {
                plugin: p.name.clone(),
                phase: "install",
                status: "skip",
                command: None,
            });
            continue;
        }
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
///
/// `store_root` is the `.aip-cli` plugins dir: a `.aip-removed` marker there
/// (or in the plugin dir itself) skips that plugin.
pub fn run_setup<R, F>(
    plugins: &[Plugin],
    runner: &R,
    linked: F,
    vendor: bool,
    store_root: &Path,
) -> std::io::Result<Vec<Step>>
where
    R: CommandRunner,
    F: Fn(&Plugin) -> bool,
{
    let mut steps = if vendor {
        run_prepare(plugins, runner, store_root)?
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
    steps.extend(run_install(plugins, runner, linked, store_root)?);
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::removed::{mark_removed, REMOVED_MARKER};
    use crate::runner::RecordingRunner;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn no_store() -> &'static Path {
        Path::new("/no-such-aip-store")
    }

    fn plugin(name: &str, prepare: bool) -> Plugin {
        Plugin {
            dir_name: name.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            has_prepare: prepare,
            path: PathBuf::from(format!("/repo/plugins/{name}")),
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
        let steps = run_prepare(&plugins, &runner, no_store()).unwrap();
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
        let steps = run_install(&plugins, &runner, |p| p.name == "a", no_store()).unwrap();
        assert_eq!(steps[0].command.as_deref(), Some("make link"));
        assert_eq!(steps[1].command.as_deref(), Some("make setup"));
    }

    #[test]
    fn full_setup_runs_prepare_then_install() {
        let plugins = vec![plugin("a", true)];
        let runner = RecordingRunner::new();
        let steps = run_setup(&plugins, &runner, |_| false, true, no_store()).unwrap();
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
        let steps = run_setup(&plugins, &runner, |_| false, false, no_store()).unwrap();
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
        let steps = run_install(&plugins, &runner, |_| false, no_store()).unwrap();
        assert_eq!(steps[0].status, "fail");
    }

    #[test]
    fn install_skips_plugin_dir_with_removed_marker() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(REMOVED_MARKER), "").unwrap();
        let plugins = vec![Plugin {
            dir_name: "a".into(),
            name: "a".into(),
            version: "1.0.0".into(),
            has_prepare: false,
            path,
        }];
        let runner = RecordingRunner::new();
        let steps = run_install(&plugins, &runner, |_| false, no_store()).unwrap();
        assert_eq!(steps[0].status, "skip");
        assert_eq!(steps[0].command, None);
        assert!(runner.lines().is_empty());
    }

    #[test]
    fn setup_skips_when_store_slot_is_marked() {
        let store = TempDir::new().unwrap();
        let slot = store.path().join("a");
        fs::create_dir_all(&slot).unwrap();
        mark_removed(&slot).unwrap();
        let plugins = vec![plugin("a", true)];
        let runner = RecordingRunner::new();
        let steps = run_setup(&plugins, &runner, |_| false, true, store.path()).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].phase, "prepare");
        assert_eq!(steps[0].status, "skip");
        assert_eq!(steps[1].phase, "install");
        assert_eq!(steps[1].status, "skip");
        assert!(runner.lines().is_empty());
    }
}
