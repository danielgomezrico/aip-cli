//! Ingesting external plugins into the [`crate::store`].
//!
//! Two sources are supported:
//! - a local folder whose immediate subdirectories are each a plugin
//!   ([`ingest_folder`]), and
//! - a remote git URL pointing at a single plugin ([`ingest_url`]).
//!
//! The clone in [`ingest_url`] flows through a [`CommandRunner`] so the glue is
//! testable without touching the network.

use crate::runner::{CommandRunner, Invocation};
use crate::store::{install_plugin, is_plugin_dir, store_name};
use std::path::{Path, PathBuf};

/// One plugin copied into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// Directory name the plugin now occupies in the store.
    pub name: String,
    /// Where it came from — a local path or a URL.
    pub source: String,
    /// Absolute destination inside the store.
    pub dest: PathBuf,
}

/// Ingest every immediate subdirectory of `folder` that is a plugin, copying
/// each into `plugins_root`. Subdirectories that are not plugins are skipped.
/// Returns one [`Ingested`] per copied plugin, sorted by directory name.
pub fn ingest_folder(folder: &Path, plugins_root: &Path) -> std::io::Result<Vec<Ingested>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(folder)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut out = Vec::new();
    for dir in dirs {
        if !is_plugin_dir(&dir) {
            continue;
        }
        let dest = install_plugin(&dir, plugins_root)?;
        out.push(Ingested {
            name: store_name(&dir).unwrap_or_default(),
            source: dir.display().to_string(),
            dest,
        });
    }
    Ok(out)
}

/// Derive a plugin directory name from a git URL: the last path segment with any
/// trailing `.git` and slashes stripped. Falls back to `"plugin"` when empty.
///
/// `https://github.com/foo/bar.git` → `bar`; `git@host:foo/baz` → `baz`.
pub fn repo_name_from_url(url: &str) -> String {
    // Drop any ?query / #fragment, then trailing slashes.
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let base = base.trim_end_matches('/');
    // Handle both `.../bar.git` and `.../bar/.git`.
    let base = base.strip_suffix("/.git").unwrap_or(base);
    let last = base.rsplit(['/', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        "plugin".to_string()
    } else {
        name.to_string()
    }
}

/// Build the `git clone --depth 1 <url> <dest>` invocation used by
/// [`ingest_url`]. Exposed so the command shape can be asserted in tests.
pub fn clone_invocation(url: &str, dest: &Path) -> Invocation {
    let dest = dest.to_string_lossy().to_string();
    Invocation::new(
        "git",
        &["clone", "--depth", "1", url, dest.as_str()],
        std::env::temp_dir(),
    )
}

/// Locate the plugin directory inside a freshly-cloned `checkout`: the checkout
/// root itself if it is a plugin, otherwise the single plugin subdirectory.
/// Errors if zero or more than one plugin is found.
pub fn find_plugin(checkout: &Path) -> std::io::Result<PathBuf> {
    if is_plugin_dir(checkout) {
        return Ok(checkout.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(checkout)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_plugin_dir(p))
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no Claude plugin found in checkout (no .claude-plugin/plugin.json)",
        )),
        1 => Ok(candidates.remove(0)),
        n => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{n} plugins found in checkout; expected exactly one"),
        )),
    }
}

/// Copy an already-cloned `checkout` into the store as the plugin from `url`.
pub fn place_clone(checkout: &Path, url: &str, plugins_root: &Path) -> std::io::Result<Ingested> {
    let src = find_plugin(checkout)?;
    let dest = install_plugin(&src, plugins_root)?;
    Ok(Ingested {
        name: store_name(&src).unwrap_or_default(),
        source: url.to_string(),
        dest,
    })
}

/// Clone `url` into a temporary checkout and copy the plugin it contains into
/// `plugins_root`. The clone runs through `runner`.
pub fn ingest_url<R: CommandRunner>(
    url: &str,
    plugins_root: &Path,
    runner: &R,
) -> std::io::Result<Ingested> {
    let tmp = tempfile::tempdir()?;
    // Name the checkout after the repo so a plugin living at the repo root keeps
    // a meaningful store name instead of a placeholder like "checkout".
    let checkout = tmp.path().join(repo_name_from_url(url));
    let out = runner.run(&clone_invocation(url, &checkout))?;
    if !out.success {
        return Err(std::io::Error::other(format!("git clone failed for {url}")));
    }
    place_clone(&checkout, url, plugins_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_plugin(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn ingest_folder_copies_plugins_skips_others() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("incoming");
        make_plugin(&src.join("alpha"), "alpha");
        make_plugin(&src.join("beta"), "beta");
        fs::create_dir_all(src.join("not-a-plugin")).unwrap();
        let store = tmp.path().join("store");

        let ingested = ingest_folder(&src, &store).unwrap();
        let names: Vec<&str> = ingested.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert!(is_plugin_dir(&store.join("alpha")));
        assert!(is_plugin_dir(&store.join("beta")));
        assert!(!store.join("not-a-plugin").exists());
    }

    #[test]
    fn ingest_folder_empty_when_none_are_plugins() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("incoming");
        fs::create_dir_all(src.join("x")).unwrap();
        let ingested = ingest_folder(&src, &tmp.path().join("store")).unwrap();
        assert!(ingested.is_empty());
    }

    #[test]
    fn repo_name_from_url_strips_git_and_path() {
        assert_eq!(repo_name_from_url("https://github.com/foo/bar.git"), "bar");
        assert_eq!(repo_name_from_url("https://github.com/foo/bar"), "bar");
        assert_eq!(repo_name_from_url("https://github.com/foo/bar/"), "bar");
        assert_eq!(repo_name_from_url("git@github.com:foo/baz.git"), "baz");
        assert_eq!(repo_name_from_url("file:///x/y/plugin-repo"), "plugin-repo");
        assert_eq!(repo_name_from_url("https://h/foo/bar.git?ref=main"), "bar");
        assert_eq!(repo_name_from_url("https://h/foo/bar/.git"), "bar");
        assert_eq!(repo_name_from_url(""), "plugin");
    }

    #[test]
    fn clone_invocation_shape() {
        let inv = clone_invocation("https://example.com/p.git", Path::new("/tmp/out"));
        assert_eq!(inv.program, "git");
        assert_eq!(
            inv.args,
            vec![
                "clone",
                "--depth",
                "1",
                "https://example.com/p.git",
                "/tmp/out"
            ]
        );
    }

    #[test]
    fn find_plugin_at_root() {
        let tmp = TempDir::new().unwrap();
        let co = tmp.path().join("co");
        make_plugin(&co, "p");
        assert_eq!(find_plugin(&co).unwrap(), co);
    }

    #[test]
    fn find_plugin_in_single_subdir() {
        let tmp = TempDir::new().unwrap();
        let co = tmp.path().join("co");
        make_plugin(&co.join("inner"), "p");
        assert_eq!(find_plugin(&co).unwrap(), co.join("inner"));
    }

    #[test]
    fn find_plugin_errors_when_absent() {
        let tmp = TempDir::new().unwrap();
        let co = tmp.path().join("co");
        fs::create_dir_all(&co).unwrap();
        assert!(find_plugin(&co).is_err());
    }

    #[test]
    fn find_plugin_errors_when_ambiguous() {
        let tmp = TempDir::new().unwrap();
        let co = tmp.path().join("co");
        make_plugin(&co.join("a"), "a");
        make_plugin(&co.join("b"), "b");
        assert!(find_plugin(&co).is_err());
    }

    #[test]
    fn place_clone_copies_into_store() {
        let tmp = TempDir::new().unwrap();
        let co = tmp.path().join("co");
        make_plugin(&co.join("my-plugin"), "my");
        let store = tmp.path().join("store");

        let i = place_clone(&co, "https://example.com/p.git", &store).unwrap();
        assert_eq!(i.name, "my-plugin");
        assert_eq!(i.source, "https://example.com/p.git");
        assert!(is_plugin_dir(&store.join("my-plugin")));
    }

    #[test]
    fn ingest_url_errors_when_clone_fails() {
        use crate::runner::RecordingRunner;
        let tmp = TempDir::new().unwrap();
        let runner = RecordingRunner::failing(|inv| inv.program == "git");
        let err = ingest_url(
            "https://example.com/p.git",
            &tmp.path().join("store"),
            &runner,
        )
        .unwrap_err();
        assert!(err.to_string().contains("git clone failed"));
    }
}
