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
use crate::removed::{is_removed, mark_removed};
use crate::runner::CommandRunner;
use crate::store::read_plugin_dirs;
use std::path::{Path, PathBuf};
use thiserror::Error;

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

/// A plugin the interactive `remove` picker can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovablePlugin {
    /// Store directory name — what [`remove_from_hosts`] resolves first.
    pub dir_name: String,
    /// Manifest `name` (falls back to [`Self::dir_name`] when unreadable).
    pub manifest: String,
    pub version: String,
}

/// Errors from resolving a `remove` picker selection.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoveSelectError {
    #[error("unknown plugin: {0}")]
    Unknown(String),
    #[error("no plugins selected")]
    Empty,
}

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

/// Store plugins that are not already marked removed, in directory-name order.
pub fn list_removable(store_root: &Path) -> Vec<RemovablePlugin> {
    read_plugin_dirs(store_root)
        .into_iter()
        .filter(|dir| !is_removed(dir))
        .filter_map(|dir| {
            let dir_name = file_name(&dir);
            if dir_name.is_empty() {
                return None;
            }
            let (manifest, version) = match PluginManifest::read(&dir) {
                Ok(m) => (m.name, m.version),
                Err(_) => (dir_name.to_string(), "?".to_string()),
            };
            Some(RemovablePlugin {
                dir_name: dir_name.to_string(),
                manifest,
                version,
            })
        })
        .collect()
}

/// Store ∪ extra-root plugins with at least one unmarked copy, deduped by
/// directory name. Manifest/version prefer the store copy, else the extra-root
/// copy. Sorted by directory name, same as [`read_plugin_dirs`].
pub fn list_removable_union<P: AsRef<Path>>(
    store_root: &Path,
    extra_roots: &[P],
) -> Vec<RemovablePlugin> {
    struct Hit {
        prefer: PathBuf,
        any_unmarked: bool,
    }
    let mut hits: std::collections::BTreeMap<String, Hit> = std::collections::BTreeMap::new();
    for dir in read_plugin_dirs(store_root) {
        let dir_name = file_name(&dir);
        if dir_name.is_empty() {
            continue;
        }
        let unmarked = !is_removed(&dir);
        hits.insert(
            dir_name.to_string(),
            Hit {
                prefer: dir,
                any_unmarked: unmarked,
            },
        );
    }
    for root in extra_roots {
        for dir in read_plugin_dirs(root.as_ref()) {
            let dir_name = file_name(&dir);
            if dir_name.is_empty() {
                continue;
            }
            let unmarked = !is_removed(&dir);
            match hits.get_mut(dir_name) {
                Some(hit) => hit.any_unmarked |= unmarked,
                None => {
                    hits.insert(
                        dir_name.to_string(),
                        Hit {
                            prefer: dir,
                            any_unmarked: unmarked,
                        },
                    );
                }
            }
        }
    }
    hits.into_iter()
        .filter(|(_, h)| h.any_unmarked)
        .map(|(dir_name, h)| {
            let (manifest, version) = match PluginManifest::read(&h.prefer) {
                Ok(m) => (m.name, m.version),
                Err(_) => (dir_name.clone(), "?".to_string()),
            };
            RemovablePlugin {
                dir_name,
                manifest,
                version,
            }
        })
        .collect()
}

/// Extra roots for the interactive remove picker: cwd `plugins/` and local
/// `origins.last` (git URLs and other non-directories are dropped).
fn remove_picker_extra_roots(store_root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut extra = Vec::new();
    if let Some(repo) = find_repo_root(cwd) {
        extra.push(plugins_root(&repo));
    }
    if let Some(last) = crate::origins::load(store_root).last {
        extra.push(PathBuf::from(last));
    }
    extra.retain(|p| p.is_dir());
    extra
}

/// Plugins the interactive `remove` picker may offer (store ∪ extra roots).
pub fn list_removable_for_remove(store_root: &Path, cwd: &Path) -> Vec<RemovablePlugin> {
    list_removable_union(store_root, &remove_picker_extra_roots(store_root, cwd))
}

fn split_selector_tokens(input: &str) -> Vec<&str> {
    input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect()
}

fn resolve_select_token(
    token: &str,
    plugins: &[RemovablePlugin],
) -> Result<String, RemoveSelectError> {
    if !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()) {
        let idx: usize = token
            .parse()
            .map_err(|_| RemoveSelectError::Unknown(token.to_string()))?;
        return plugins
            .get(idx.wrapping_sub(1))
            .filter(|_| idx >= 1)
            .map(|p| p.dir_name.clone())
            .ok_or_else(|| RemoveSelectError::Unknown(token.to_string()));
    }
    if let Some(p) = plugins.iter().find(|p| p.dir_name == token) {
        return Ok(p.dir_name.clone());
    }
    let hits: Vec<_> = plugins.iter().filter(|p| p.manifest == token).collect();
    if let [p] = hits.as_slice() {
        return Ok(p.dir_name.clone());
    }
    Err(RemoveSelectError::Unknown(token.to_string()))
}

/// Resolve a picker line (names or 1-based numbers, comma/space-separated)
/// to store directory names. Unknown tokens error.
pub fn parse_remove_selectors(
    input: &str,
    plugins: &[RemovablePlugin],
) -> Result<Vec<String>, RemoveSelectError> {
    let tokens = split_selector_tokens(input);
    if tokens.is_empty() {
        return Err(RemoveSelectError::Empty);
    }
    let mut chosen = Vec::new();
    for tok in tokens {
        let name = resolve_select_token(tok, plugins)?;
        if !chosen.iter().any(|c| c == &name) {
            chosen.push(name);
        }
    }
    Ok(chosen)
}

/// Resolve CLI name tokens against the removable list. Known dir names,
/// unique manifest names, and 1-based indices expand; anything else is kept
/// as-is so `name@marketplace` and host-only plugins still work.
pub fn expand_remove_names(names: &[String], plugins: &[RemovablePlugin]) -> Vec<String> {
    let mut out = Vec::new();
    for n in names {
        let resolved = resolve_select_token(n, plugins).unwrap_or_else(|_| n.clone());
        if !out.iter().any(|c| c == &resolved) {
            out.push(resolved);
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

    #[test]
    fn list_removable_empty_store() {
        let store = TempDir::new().unwrap();
        assert!(list_removable(store.path()).is_empty());
    }

    #[test]
    fn list_removable_skips_marked_and_keeps_order() {
        let store = TempDir::new().unwrap();
        write_store_plugin(store.path(), "alpha", "alpha", None);
        write_store_plugin(store.path(), "beta", "beta-plugin", None);
        write_store_plugin(store.path(), "gamma", "gamma", None);
        mark_removed(&store.path().join("beta")).unwrap();
        let listed = list_removable(store.path());
        assert_eq!(
            listed
                .iter()
                .map(|p| (p.dir_name.as_str(), p.manifest.as_str(), p.version.as_str()))
                .collect::<Vec<_>>(),
            vec![("alpha", "alpha", "1.0.0"), ("gamma", "gamma", "1.0.0")]
        );
    }

    #[test]
    fn list_removable_unreadable_manifest_uses_dir_name() {
        let store = TempDir::new().unwrap();
        let meta = store.path().join("foo").join(".claude-plugin");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("plugin.json"), "not-json").unwrap();
        let listed = list_removable(store.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dir_name, "foo");
        assert_eq!(listed[0].manifest, "foo");
        assert_eq!(listed[0].version, "?");
    }

    fn listed_dir_names(listed: &[RemovablePlugin]) -> Vec<&str> {
        listed.iter().map(|p| p.dir_name.as_str()).collect()
    }

    #[test]
    fn list_removable_union_source_only_when_store_empty() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let extra = tmp.path().join("src");
        write_store_plugin(&extra, "job-hunter", "job-hunter", None);
        let listed = list_removable_union(&store, &[extra.as_path()]);
        assert_eq!(listed_dir_names(&listed), vec!["job-hunter"]);
        assert!(list_removable(&store).is_empty());
    }

    #[test]
    fn list_removable_union_dedups_same_dir_name() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        let extra = tmp.path().join("src");
        write_store_plugin(&store, "job-hunter", "store-name", None);
        write_store_plugin(&extra, "job-hunter", "source-name", None);
        let listed = list_removable_union(&store, &[extra.as_path()]);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dir_name, "job-hunter");
        assert_eq!(listed[0].manifest, "store-name");
    }

    #[test]
    fn list_removable_union_includes_store_marked_source_unmarked() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        let extra = tmp.path().join("src");
        write_store_plugin(&store, "job-hunter", "job-hunter", None);
        write_store_plugin(&extra, "job-hunter", "job-hunter", None);
        mark_removed(&store.join("job-hunter")).unwrap();
        assert!(crate::removed::is_setup_blocked(
            &extra.join("job-hunter"),
            &store.join("job-hunter")
        ));
        let listed = list_removable_union(&store, &[extra.as_path()]);
        assert_eq!(listed_dir_names(&listed), vec!["job-hunter"]);
        assert!(list_removable(&store).is_empty());
    }

    #[test]
    fn list_removable_union_omits_when_all_known_copies_marked() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        let extra = tmp.path().join("src");
        write_store_plugin(&extra, "job-hunter", "job-hunter", None);
        mark_removed(&extra.join("job-hunter")).unwrap();
        assert!(list_removable_union(&store, &[extra.as_path()]).is_empty());

        write_store_plugin(&store, "job-hunter", "job-hunter", None);
        mark_removed(&store.join("job-hunter")).unwrap();
        assert!(list_removable_union(&store, &[extra.as_path()]).is_empty());
        assert!(list_removable(&store).is_empty());
    }

    #[test]
    fn list_removable_union_includes_store_unmarked_when_source_marked() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        let extra = tmp.path().join("src");
        write_store_plugin(&store, "alpha", "alpha", None);
        write_store_plugin(&extra, "alpha", "alpha", None);
        mark_removed(&extra.join("alpha")).unwrap();
        let listed = list_removable_union(&store, &[extra.as_path()]);
        assert_eq!(listed_dir_names(&listed), vec!["alpha"]);
    }

    #[test]
    fn list_removable_for_remove_uses_cwd_plugins_and_local_last() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let repo = tmp.path().join("repo");
        write_store_plugin(&repo.join("plugins"), "job-hunter", "job-hunter", None);
        crate::origins::record_last(&store, "https://example.com/plugins.git").unwrap();
        let listed = list_removable_for_remove(&store, &repo);
        assert_eq!(listed_dir_names(&listed), vec!["job-hunter"]);

        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let last = tmp.path().join("last-src");
        write_store_plugin(&last, "stock-advisor", "stock-advisor", None);
        crate::origins::record_last(&store, &last.to_string_lossy()).unwrap();
        let listed = list_removable_for_remove(&store, &elsewhere);
        assert_eq!(listed_dir_names(&listed), vec!["stock-advisor"]);
    }

    fn sample_plugins() -> Vec<RemovablePlugin> {
        vec![
            RemovablePlugin {
                dir_name: "flutter".into(),
                manifest: "flutter-pivara".into(),
                version: "1.0.0".into(),
            },
            RemovablePlugin {
                dir_name: "frontend".into(),
                manifest: "frontend".into(),
                version: "2.0.0".into(),
            },
            RemovablePlugin {
                dir_name: "a".into(),
                manifest: "shared".into(),
                version: "1.0.0".into(),
            },
            RemovablePlugin {
                dir_name: "b".into(),
                manifest: "shared".into(),
                version: "1.0.0".into(),
            },
        ]
    }

    #[test]
    fn parse_selectors_by_index_and_name() {
        let plugins = sample_plugins();
        assert_eq!(
            parse_remove_selectors("1", &plugins).unwrap(),
            vec!["flutter"]
        );
        assert_eq!(
            parse_remove_selectors("2 1", &plugins).unwrap(),
            vec!["frontend", "flutter"]
        );
        assert_eq!(
            parse_remove_selectors("1,2", &plugins).unwrap(),
            vec!["flutter", "frontend"]
        );
        assert_eq!(
            parse_remove_selectors("frontend", &plugins).unwrap(),
            vec!["frontend"]
        );
        assert_eq!(
            parse_remove_selectors("flutter-pivara", &plugins).unwrap(),
            vec!["flutter"]
        );
    }

    #[test]
    fn parse_selectors_dedups_and_rejects() {
        let plugins = sample_plugins();
        assert_eq!(
            parse_remove_selectors("1 1 frontend", &plugins).unwrap(),
            vec!["flutter", "frontend"]
        );
        assert_eq!(
            parse_remove_selectors("", &plugins).unwrap_err(),
            RemoveSelectError::Empty
        );
        assert_eq!(
            parse_remove_selectors("   ", &plugins).unwrap_err(),
            RemoveSelectError::Empty
        );
        assert_eq!(
            parse_remove_selectors("0", &plugins).unwrap_err(),
            RemoveSelectError::Unknown("0".into())
        );
        assert_eq!(
            parse_remove_selectors("99", &plugins).unwrap_err(),
            RemoveSelectError::Unknown("99".into())
        );
        assert_eq!(
            parse_remove_selectors("missing", &plugins).unwrap_err(),
            RemoveSelectError::Unknown("missing".into())
        );
        assert_eq!(
            parse_remove_selectors("shared", &plugins).unwrap_err(),
            RemoveSelectError::Unknown("shared".into())
        );
    }

    #[test]
    fn expand_names_resolves_known_and_passthrough_unknown() {
        let plugins = sample_plugins();
        assert_eq!(
            expand_remove_names(
                &[
                    "1".into(),
                    "flutter-pivara".into(),
                    "missing".into(),
                    "foo@bar".into(),
                    "1".into(),
                ],
                &plugins
            ),
            vec!["flutter", "missing", "foo@bar"]
        );
    }
}
