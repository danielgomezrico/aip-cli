//! Local skip marker written by `aip-cli remove`.
//!
//! A gitignored `.aip-removed` in a plugin folder tells `setup` / ingest not
//! to reinstall that plugin. Delete the file to allow setup again.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// File name written into a plugin directory to skip later setup.
pub const REMOVED_MARKER: &str = ".aip-removed";

const MARKER_BODY: &str = "# Delete this file to allow `aip-cli setup` to reinstall this plugin.\n";

/// Path of the skip marker inside `dir`.
pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join(REMOVED_MARKER)
}

/// True when `dir` contains a skip marker file.
pub fn is_removed(dir: &Path) -> bool {
    marker_path(dir).is_file()
}

/// True when setup must not install this plugin: the plugin dir itself or its
/// store slot has a skip marker.
pub fn is_setup_blocked(plugin_dir: &Path, store_slot: &Path) -> bool {
    is_removed(plugin_dir) || is_removed(store_slot)
}

/// Write the skip marker in `dir` and gitignore it when `dir` is in a git repo.
/// Returns the marker path. Idempotent.
pub fn mark_removed(dir: &Path) -> io::Result<PathBuf> {
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not a directory", dir.display()),
        ));
    }
    let path = marker_path(dir);
    if !path.is_file() {
        std::fs::write(&path, MARKER_BODY)?;
    }
    let _ = ensure_gitignored(dir);
    Ok(path)
}

fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .current_dir(dir)
        .stderr(Stdio::null());
    cmd
}

/// `Some(true)` ignored, `Some(false)` tracked/unignored, `None` not a git repo.
fn git_is_ignored(dir: &Path, rel: &str) -> Option<bool> {
    let status = git_cmd(dir)
        .args(["check-ignore", "-q", "--", rel])
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn git_exclude_path(dir: &Path) -> Option<PathBuf> {
    let out = git_cmd(dir)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let p = PathBuf::from(raw.trim());
    if p.as_os_str().is_empty() {
        return None;
    }
    if p.is_absolute() {
        Some(p)
    } else {
        Some(dir.join(p))
    }
}

/// Append `.aip-removed` to this repo's local exclude if it is not already ignored.
fn ensure_gitignored(dir: &Path) -> io::Result<()> {
    match git_is_ignored(dir, REMOVED_MARKER) {
        Some(true) | None => return Ok(()),
        Some(false) => {}
    }
    let exclude = match git_exclude_path(dir) {
        Some(p) => p,
        None => return Ok(()),
    };
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = if exclude.is_file() {
        std::fs::read_to_string(&exclude)?
    } else {
        String::new()
    };
    if body.lines().any(|l| l.trim() == REMOVED_MARKER) {
        return Ok(());
    }
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(REMOVED_MARKER);
    body.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&exclude)?;
    f.write_all(body.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn git_init(dir: &Path) {
        let st = git_cmd(dir)
            .args(["init", "--quiet"])
            .status()
            .expect("spawn git init");
        assert!(st.success(), "git init failed");
    }

    fn git_check_ignore(dir: &Path, rel: &str) -> Option<bool> {
        git_is_ignored(dir, rel)
    }

    #[test]
    fn is_removed_false_when_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_removed(tmp.path()));
    }

    #[test]
    fn mark_removed_writes_marker() {
        let tmp = TempDir::new().unwrap();
        let path = mark_removed(tmp.path()).unwrap();
        assert_eq!(path, tmp.path().join(REMOVED_MARKER));
        assert!(is_removed(tmp.path()));
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("aip-cli setup"), "{body}");
    }

    #[test]
    fn mark_removed_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let a = mark_removed(tmp.path()).unwrap();
        let first = fs::read_to_string(&a).unwrap();
        let b = mark_removed(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(fs::read_to_string(&b).unwrap(), first);
    }

    #[test]
    fn mark_removed_errors_on_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let err = mark_removed(&tmp.path().join("nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn is_setup_blocked_by_plugin_dir_or_store_slot() {
        let tmp = TempDir::new().unwrap();
        let plugin = tmp.path().join("plugin");
        let slot = tmp.path().join("slot");
        fs::create_dir_all(&plugin).unwrap();
        fs::create_dir_all(&slot).unwrap();
        assert!(!is_setup_blocked(&plugin, &slot));
        mark_removed(&plugin).unwrap();
        assert!(is_setup_blocked(&plugin, &slot));

        let plugin2 = tmp.path().join("plugin2");
        fs::create_dir_all(&plugin2).unwrap();
        mark_removed(&slot).unwrap();
        assert!(is_setup_blocked(&plugin2, &slot));
    }

    #[test]
    fn mark_removed_without_git_still_writes() {
        let tmp = TempDir::new().unwrap();
        mark_removed(tmp.path()).unwrap();
        assert!(is_removed(tmp.path()));
        assert_eq!(git_check_ignore(tmp.path(), REMOVED_MARKER), None);
    }

    #[test]
    fn git_cmd_discards_stderr_on_non_repo() {
        let tmp = TempDir::new().unwrap();
        let out = git_cmd(tmp.path())
            .args(["check-ignore", "-q", "--", REMOVED_MARKER])
            .output()
            .expect("spawn git");
        assert!(
            out.stderr.is_empty(),
            "git stderr should be discarded, got {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(git_is_ignored(tmp.path(), REMOVED_MARKER), None);
    }

    #[test]
    fn mark_removed_is_gitignored_in_a_repo() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());
        let plugin = tmp.path().join("plugins").join("foo");
        fs::create_dir_all(&plugin).unwrap();
        mark_removed(&plugin).unwrap();
        assert_eq!(git_check_ignore(&plugin, REMOVED_MARKER), Some(true));
        let out = git_cmd(&plugin)
            .args(["check-ignore", "-v", "--", REMOVED_MARKER])
            .output()
            .unwrap();
        assert!(out.status.success(), "check-ignore should match");
        let line = String::from_utf8_lossy(&out.stdout);
        assert!(
            line.contains(REMOVED_MARKER),
            "expected exclude match, got {line}"
        );
    }

    #[test]
    fn ensure_gitignored_appends_exclude_once() {
        let tmp = TempDir::new().unwrap();
        git_init(tmp.path());
        let plugin = tmp.path().join("p");
        fs::create_dir_all(&plugin).unwrap();
        mark_removed(&plugin).unwrap();
        mark_removed(&plugin).unwrap();
        let exclude = git_exclude_path(&plugin).unwrap();
        let body = fs::read_to_string(&exclude).unwrap();
        let hits = body.lines().filter(|l| l.trim() == REMOVED_MARKER).count();
        assert_eq!(hits, 1, "{body}");
    }
}
