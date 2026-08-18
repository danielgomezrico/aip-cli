//! Uninstall a named plugin from Claude Code and Codex CLI.
//!
//! Resolution and host dispatch, then a gitignored `.aip-removed` in each
//! matching plugin folder so later `setup` / ingest skip it. Does not delete
//! from the aip-cli store.

use crate::claude_plugins::{resolve_marketplace_name, uninstall_invocation};
use crate::codex_plugins::remove_invocation;
use crate::config::find_repo_root;
use crate::discovery::plugins_root;
use crate::manifest::PluginManifest;
use crate::removed::mark_removed;
use crate::runner::CommandRunner;
use crate::store::read_plugin_dirs;
use std::path::{Path, PathBuf};

pub struct HostAttempt {
    pub program: &'static str,
    pub success: bool,
}

pub struct RemoveReport {
    pub spec: String,
    pub attempts: Vec<HostAttempt>,
    /// Marker files written so later setup skips these plugin folders.
    pub marked: Vec<PathBuf>,
}

pub const HOSTS: [&str; 2] = ["claude", "codex"];
pub const NEITHER_HOST_ERR: &str = "neither claude nor codex found on PATH";

/// Resolve `name` to the spec both hosts receive. `@` is pass-through; otherwise
/// store dir-name wins, then a unique readable manifest name, else the raw token.
fn resolve_spec(name: &str, store_root: &Path) -> String {
    if name.contains('@') {
        return name.to_string();
    }
    let dirs = read_plugin_dirs(store_root);
    if let Some(dir) = dirs
        .iter()
        .find(|d| d.file_name().and_then(|s| s.to_str()) == Some(name))
    {
        let manifest = match PluginManifest::read(dir) {
            Ok(m) => m.name,
            Err(_) => name.to_string(),
        };
        let marketplace = resolve_marketplace_name(dir, name);
        return format!("{manifest}@{marketplace}");
    }
    let hits: Vec<_> = dirs
        .iter()
        .filter(|dir| {
            PluginManifest::read(dir)
                .ok()
                .is_some_and(|m| m.name == name)
        })
        .collect();
    if let [dir] = hits.as_slice() {
        let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or(name);
        let marketplace = resolve_marketplace_name(dir, dir_name);
        return format!("{name}@{marketplace}");
    }
    name.to_string()
}

fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("")
}

fn lookup_token(name: &str) -> &str {
    name.split('@').next().unwrap_or(name)
}

/// Plugin directory under `root` matching `name` (dir name, then unique manifest).
fn find_plugin_dir(name: &str, root: &Path) -> Option<PathBuf> {
    let dirs = read_plugin_dirs(root);
    if let Some(dir) = dirs.iter().find(|d| file_name(d) == name) {
        return Some(dir.clone());
    }
    let token = lookup_token(name);
    if token != name {
        if let Some(dir) = dirs.iter().find(|d| file_name(d) == token) {
            return Some(dir.clone());
        }
    }
    let hits: Vec<_> = dirs
        .iter()
        .filter(|dir| {
            PluginManifest::read(dir)
                .ok()
                .is_some_and(|m| m.name == token)
        })
        .collect();
    if let [dir] = hits.as_slice() {
        Some((*dir).clone())
    } else {
        None
    }
}

/// Store plugin dir and, when `cwd` is inside a plugins repo, the source dir.
pub fn resolve_plugin_dirs(name: &str, store_root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |dir: PathBuf| {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if out
            .iter()
            .any(|p: &PathBuf| p.canonicalize().unwrap_or_else(|_| p.clone()) == key)
        {
            return;
        }
        out.push(dir);
    };
    if let Some(d) = find_plugin_dir(name, store_root) {
        push(d);
    }
    if let Some(repo) = find_repo_root(cwd) {
        let root = plugins_root(&repo);
        if let Some(d) = find_plugin_dir(name, &root) {
            push(d);
        }
    }
    out
}

pub fn remove_from_hosts<R, F>(
    runner: &R,
    exists: F,
    name: &str,
    store_root: &Path,
    cwd: &Path,
) -> RemoveReport
where
    R: CommandRunner + ?Sized,
    F: Fn(&str) -> bool,
{
    let spec = resolve_spec(name, store_root);
    let mut attempts = Vec::new();
    for program in HOSTS {
        if !exists(program) {
            continue;
        }
        let inv = match program {
            "claude" => uninstall_invocation(&spec, cwd),
            "codex" => remove_invocation(&spec, cwd),
            _ => continue,
        };
        let success = match runner.run(&inv) {
            Ok(o) => o.success,
            Err(_) => false,
        };
        attempts.push(HostAttempt { program, success });
    }
    let mut marked = Vec::new();
    for dir in resolve_plugin_dirs(name, store_root, cwd) {
        if let Ok(path) = mark_removed(&dir) {
            marked.push(path);
        }
    }
    RemoveReport {
        spec,
        attempts,
        marked,
    }
}

pub fn no_hosts_attempted(report: &RemoveReport) -> bool {
    report.attempts.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Invocation, Outcome, RecordingRunner};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn write_store_plugin(store: &Path, dir: &str, manifest: &str, marketplace: Option<&str>) {
        let meta = store.join(dir).join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("plugin.json"),
            format!(r#"{{"name":"{manifest}","version":"1.0.0"}}"#),
        )
        .unwrap();
        if let Some(m) = marketplace {
            std::fs::write(
                meta.join("marketplace.json"),
                format!(r#"{{"name":"{m}","plugins":[]}}"#),
            )
            .unwrap();
        }
    }

    fn both_on_path(_: &str) -> bool {
        true
    }

    fn run(
        runner: &RecordingRunner,
        exists: impl Fn(&str) -> bool,
        name: &str,
        store: &Path,
    ) -> RemoveReport {
        remove_from_hosts(runner, exists, name, store, Path::new("/cwd"))
    }

    struct RecordThenErr {
        calls: Mutex<Vec<Invocation>>,
        err_program: &'static str,
    }

    impl CommandRunner for RecordThenErr {
        // MSRV 1.80: Error::other is 1.81+
        #[allow(clippy::io_other_error)]
        fn run(&self, inv: &Invocation) -> io::Result<Outcome> {
            self.calls.lock().unwrap().push(inv.clone());
            if inv.program == self.err_program {
                return Err(io::Error::new(io::ErrorKind::Other, "injected"));
            }
            Ok(Outcome::ok())
        }
    }

    #[test]
    fn both_hosts_claude_then_codex() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let spec = "flutter-pivara@flutter-pivara";
        let report = run(&runner, both_on_path, spec, store.path());
        assert_eq!(report.spec, spec);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].program, "claude");
        assert_eq!(calls[0].args, ["plugin", "uninstall", spec]);
        assert_eq!(calls[1].program, "codex");
        assert_eq!(calls[1].args, ["plugin", "remove", spec]);
        assert_eq!(runner.cwds(), [PathBuf::from("/cwd")]);
        assert_eq!(report.attempts.len(), 2);
        assert!(report.attempts.iter().all(|a| a.success));
    }

    #[test]
    fn skips_host_not_on_path() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, |p| p == "claude", "x", store.path());
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "claude");
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].program, "claude");

        let runner = RecordingRunner::new();
        let report = run(&runner, |p| p == "codex", "x", store.path());
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "codex");
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(report.attempts[0].program, "codex");
    }

    #[test]
    fn neither_host_empty_attempts() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, |_| false, "x", store.path());
        assert!(report.attempts.is_empty());
        assert!(runner.calls().is_empty());
        assert!(no_hosts_attempted(&report));
    }

    #[test]
    fn no_hosts_attempted_is_only_failure() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::failing(|i| i.program == "claude");
        let report = run(&runner, both_on_path, "x", store.path());
        assert_eq!(report.attempts.len(), 2);
        assert!(!report.attempts[0].success);
        assert!(report.attempts[1].success);
        assert!(!no_hosts_attempted(&report));
    }

    #[test]
    fn one_host_fail_still_records_other() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::failing(|i| i.program == "claude");
        let _ = run(&runner, both_on_path, "x", store.path());
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(runner.calls()[1].program, "codex");
    }

    #[test]
    fn no_grok_or_pi_or_marketplace_remove() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let _ = run(&runner, both_on_path, "x", store.path());
        for line in runner.lines() {
            assert!(!line.starts_with("grok"), "{line}");
            assert!(!line.starts_with("pi "), "{line}");
            assert!(!line.contains(concat!("marketplace", " remove")), "{line}");
        }
    }

    #[test]
    fn store_plugin_dir_remains() {
        let store = TempDir::new().unwrap();
        write_store_plugin(
            store.path(),
            "flutter",
            "flutter-pivara",
            Some("flutter-pivara"),
        );
        let plugin = store.path().join("flutter");
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "flutter", store.path());
        assert!(plugin.is_dir());
        assert!(plugin.join(".claude-plugin").join("plugin.json").is_file());
        assert!(plugin.join(crate::removed::REMOVED_MARKER).is_file());
        assert_eq!(
            report.marked,
            vec![plugin.join(crate::removed::REMOVED_MARKER)]
        );
    }

    #[test]
    fn marks_source_plugin_when_cwd_is_repo() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let plugins = repo.join("plugins");
        write_store_plugin(
            &plugins,
            "flutter",
            "flutter-pivara",
            Some("flutter-pivara"),
        );
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let runner = RecordingRunner::new();
        let report = remove_from_hosts(&runner, both_on_path, "flutter", &store, &repo);
        let marker = plugins.join("flutter").join(crate::removed::REMOVED_MARKER);
        assert!(marker.is_file());
        assert_eq!(report.marked, vec![marker]);
    }

    #[test]
    fn marks_store_and_source_when_both_exist() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        write_store_plugin(
            &repo.join("plugins"),
            "flutter",
            "flutter-pivara",
            Some("flutter-pivara"),
        );
        let store = tmp.path().join("store");
        write_store_plugin(&store, "flutter", "flutter-pivara", Some("flutter-pivara"));
        let runner = RecordingRunner::new();
        let report = remove_from_hosts(&runner, both_on_path, "flutter", &store, &repo);
        assert_eq!(report.marked.len(), 2);
        assert!(store
            .join("flutter")
            .join(crate::removed::REMOVED_MARKER)
            .is_file());
        assert!(repo
            .join("plugins")
            .join("flutter")
            .join(crate::removed::REMOVED_MARKER)
            .is_file());
    }

    #[test]
    fn at_token_marks_matching_dir() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "foo", "foo", Some("bar"));
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "foo@bar", store.path());
        assert!(store
            .path()
            .join("foo")
            .join(crate::removed::REMOVED_MARKER)
            .is_file());
        assert_eq!(report.marked.len(), 1);
    }

    #[test]
    fn missing_plugin_writes_no_marker() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "missing", store.path());
        assert!(report.marked.is_empty());
    }

    #[test]
    fn empty_store_uses_raw_token() {
        let store = TempDir::new().unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "missing", store.path());
        assert_eq!(report.spec, "missing");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args[2], "missing");
        assert_eq!(calls[1].args[2], "missing");
    }

    #[test]
    fn at_token_passthrough() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "foo", "foo", Some("foo"));
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "foo@bar", store.path());
        assert_eq!(report.spec, "foo@bar");
        assert_eq!(runner.calls()[0].args[2], "foo@bar");
        assert_eq!(runner.calls()[1].args[2], "foo@bar");
    }

    #[test]
    fn dir_name_resolves_manifest_at_marketplace() {
        let store = TempDir::new().unwrap();
        write_store_plugin(
            store.path(),
            "flutter",
            "flutter-pivara",
            Some("flutter-pivara"),
        );
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "flutter", store.path());
        let spec = "flutter-pivara@flutter-pivara";
        assert_eq!(report.spec, spec);
        assert_eq!(runner.calls()[0].args, ["plugin", "uninstall", spec]);
        assert_eq!(runner.calls()[1].args, ["plugin", "remove", spec]);
    }

    #[test]
    fn unique_manifest_name_hit() {
        let store = TempDir::new().unwrap();
        write_store_plugin(
            store.path(),
            "flutter",
            "flutter-pivara",
            Some("flutter-pivara"),
        );
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "flutter-pivara", store.path());
        assert_eq!(report.spec, "flutter-pivara@flutter-pivara");
    }

    #[test]
    fn zero_manifest_hits_uses_raw_token() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "other", "other", Some("other"));
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "missing", store.path());
        assert_eq!(report.spec, "missing");
        assert_eq!(runner.calls()[0].args[2], "missing");
    }

    #[test]
    fn ambiguous_manifest_uses_raw_token() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "a", "shared", Some("ma"));
        write_store_plugin(store.path(), "b", "shared", Some("mb"));
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "shared", store.path());
        assert_eq!(report.spec, "shared");
    }

    #[test]
    fn dir_name_wins_over_other_manifest() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "alpha", "other", Some("other"));
        write_store_plugin(store.path(), "beta", "alpha", Some("beta-mp"));
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "alpha", store.path());
        assert_eq!(report.spec, "other@other");
    }

    #[test]
    fn unreadable_manifest_on_dir_hit_uses_dir_name() {
        let store = TempDir::new().unwrap();
        let meta = store.path().join("foo").join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("plugin.json"), "not-json").unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "foo", store.path());
        assert_eq!(report.spec, "foo@foo");
    }

    #[test]
    fn missing_marketplace_json_falls_back_to_dir_name() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "flutter", "flutter-pivara", None);
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "flutter", store.path());
        assert_eq!(report.spec, "flutter-pivara@flutter");
    }

    #[test]
    fn root_plugin_json_is_invisible() {
        let store = TempDir::new().unwrap();
        let dir = store.path().join("flutter");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"name":"flutter-pivara","version":"1.0.0"}"#,
        )
        .unwrap();
        let runner = RecordingRunner::new();
        let report = run(&runner, both_on_path, "flutter", store.path());
        assert_eq!(report.spec, "flutter");
    }

    #[test]
    fn runner_err_counts_as_failed_attempt() {
        let store = TempDir::new().unwrap();
        let runner = RecordThenErr {
            calls: Mutex::new(Vec::new()),
            err_program: "claude",
        };
        let report = remove_from_hosts(&runner, both_on_path, "x", store.path(), Path::new("/cwd"));
        assert_eq!(report.attempts.len(), 2);
        assert_eq!(report.attempts[0].program, "claude");
        assert!(!report.attempts[0].success);
        assert_eq!(report.attempts[1].program, "codex");
        assert!(report.attempts[1].success);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].program, "codex");
    }

    #[test]
    fn hosts_are_not_all_targets() {
        assert_eq!(HOSTS, ["claude", "codex"]);
        assert!(!HOSTS.contains(&"grok"));
        assert!(!HOSTS.contains(&"pi"));
    }
}
