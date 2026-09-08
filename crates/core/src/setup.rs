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

pub fn install_pi_packages<R: CommandRunner>(
    plugins: &[Plugin],
    store_root: &Path,
    runner: &R,
) -> HostInstallReport {
    let catalog = crate::host_models::PiModelCatalog::detect();
    install_pi_packages_with_catalog(plugins, store_root, runner, &catalog)
}

fn install_pi_packages_with_catalog<R: CommandRunner>(
    plugins: &[Plugin],
    store_root: &Path,
    runner: &R,
    catalog: &crate::host_models::PiModelCatalog,
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
        let mut old_sources = vec![abs.clone()];
        if let Ok(store_source) = std::fs::canonicalize(store_root.join(&p.dir_name)) {
            if store_source != abs {
                old_sources.push(store_source);
            }
        }
        for source in old_sources {
            let source_text = source.to_string_lossy().into_owned();
            let remove = Invocation::new("pi", &["remove", source_text.as_str()], &source);
            let _ = runner.run(&remove);
        }
        let install_root = crate::host_models::stage_for_host(
            store_root,
            &abs,
            &p.dir_name,
            crate::host_models::Host::Pi,
        )
        .unwrap_or(abs);
        crate::host_models::prepare_pi_package(&install_root, &p.dir_name, catalog);
        let abs_str = install_root.to_string_lossy().into_owned();
        let inv = Invocation::new("pi", &["install", abs_str.as_str()], &install_root);
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
    use crate::removed::{mark_removed, REMOVED_MARKER};
    use crate::runner::{CommandRunner, Invocation, Outcome, RecordingRunner};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;
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

    fn pi_catalog() -> crate::host_models::PiModelCatalog {
        crate::host_models::PiModelCatalog::parse(
            "provider model context max-out thinking images\n\
             ollama muse-glimmer 131K 8K yes yes\n\
             ollama qwen3.8-27b 131K 8K no no\n\
             ollama qwen3-coder:30b 16K 8K no no\n\
             ollama qwen2.5-coder:7b 16K 8K no no\n",
        )
    }

    fn install_for_test<R: CommandRunner>(
        plugins: &[Plugin],
        store: &Path,
        runner: &R,
    ) -> HostInstallReport {
        install_pi_packages_with_catalog(plugins, store, runner, &pi_catalog())
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

    #[test]
    fn pi_install_removes_each_source_before_installing_its_host_stage() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let plugins = vec![temp_plugin("a", tmp.path()), temp_plugin("b", tmp.path())];
        let runner = RecordingRunner::new();
        let report = install_for_test(&plugins, &store, &runner);
        assert_eq!(report.ok, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.attempted, 2);
        let calls = runner.calls();
        assert_eq!(calls.len(), 4);
        for (i, p) in plugins.iter().enumerate() {
            let staged = crate::host_models::host_stage_dir(
                &store,
                crate::host_models::Host::Pi,
                &p.dir_name,
            );
            let remove = &calls[i * 2];
            let install = &calls[i * 2 + 1];
            assert_eq!(remove.args[0], "remove");
            assert_eq!(Path::new(&remove.args[1]), p.path.canonicalize().unwrap());
            assert_eq!(install.program, "pi");
            assert_eq!(
                install.args,
                vec!["install".to_string(), staged.to_string_lossy().into_owned()]
            );
            assert!(Path::new(&install.args[1]).is_absolute());
            assert_eq!(install.cwd, staged);
        }
    }

    #[test]
    fn pi_install_relative_path_becomes_abs() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let name = "relplug";
        let abs_dir = tmp.path().join(name);
        fs::create_dir_all(&abs_dir).unwrap();
        let mut p = temp_plugin(name, tmp.path());
        p.path = abs_dir.join(".").join("..").join(name);
        let runner = RecordingRunner::new();
        let report = install_for_test(&[p], &store, &runner);
        assert_eq!(report.ok, 1);
        assert_eq!(report.skipped, 0);
        let arg = &runner.calls()[1].args[1];
        assert!(Path::new(arg).is_absolute());
        assert!(!arg.contains("/./") && !arg.contains("/../"));
        assert_eq!(
            Path::new(arg),
            crate::host_models::host_stage_dir(&store, crate::host_models::Host::Pi, name)
        );
    }

    #[test]
    fn pi_install_skips_missing_path() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let good = temp_plugin("good", tmp.path());
        let bad = Plugin {
            dir_name: "missing".into(),
            name: "missing".into(),
            version: "1.0.0".into(),
            has_prepare: false,
            path: tmp.path().join("does-not-exist"),
        };
        let runner = RecordingRunner::new();
        let report = install_for_test(&[good.clone(), bad], &store, &runner);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.ok, 1);
        assert_eq!(report.attempted, 1);
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(
            runner.calls()[1].args[1],
            crate::host_models::host_stage_dir(
                &store,
                crate::host_models::Host::Pi,
                &good.dir_name,
            )
            .to_string_lossy()
            .into_owned()
        );
    }

    #[test]
    fn pi_install_counts_runner_failures() {
        let tmp = TempDir::new().unwrap();
        let plugins = vec![temp_plugin("a", tmp.path()), temp_plugin("b", tmp.path())];
        let runner = RecordingRunner::failing(|_| true);
        let report = install_for_test(&plugins, tmp.path(), &runner);
        assert_eq!(report.failed, 2);
        assert_eq!(report.attempted, 2);
        assert_eq!(report.ok, 0);
        assert_eq!(report.skipped, 0);
    }

    #[test]
    fn pi_install_counts_run_err() {
        let tmp = TempDir::new().unwrap();
        let plugins = vec![temp_plugin("a", tmp.path())];
        let runner = ErrRunner::new();
        let report = install_for_test(&plugins, tmp.path(), &runner);
        assert_eq!(report.failed, 1);
        assert_eq!(report.attempted, 1);
        assert_eq!(report.ok, 0);
        assert_eq!(runner.calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn pi_install_empty() {
        let runner = RecordingRunner::new();
        let report = install_for_test(&[], Path::new("/no-store"), &runner);
        assert_eq!(report, HostInstallReport::default());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn pi_install_rewrites_agent_models_on_stage() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let p = temp_plugin("lead", tmp.path());
        fs::create_dir_all(p.path.join("agents")).unwrap();
        fs::write(
            p.path.join("agents").join("lead.md"),
            "---\nmodel: opus\n---\n# Lead\n",
        )
        .unwrap();
        let runner = RecordingRunner::new();
        let _ = install_for_test(std::slice::from_ref(&p), &store, &runner);
        let staged =
            crate::host_models::host_stage_dir(&store, crate::host_models::Host::Pi, &p.dir_name);
        let staged_text = fs::read_to_string(staged.join("agents").join("lead.md")).unwrap();
        let src_text = fs::read_to_string(p.path.join("agents").join("lead.md")).unwrap();
        assert!(staged_text.contains("model: ollama/muse-glimmer"));
        assert!(src_text.contains("model: opus"));
        assert!(!src_text.contains("*opus*"));
    }
}
