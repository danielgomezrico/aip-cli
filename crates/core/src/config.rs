//! Filesystem locations: repo root discovery and the per-user config directory.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Name of the per-directory marker that records the mode for `aip-cli enable`.
pub const MARKER_NAME: &str = ".aip-cli.toml";

/// The per-user config dir (platform-specific): `~/.config/aip-cli` on
/// Linux, `~/Library/Application Support/aip-cli` on macOS.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("aip-cli")
}

/// Walk up from `start` to find a repo root: the first ancestor containing a
/// `plugins/` directory, or failing that one containing `.git`.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut best_git: Option<PathBuf> = None;
    for dir in start.ancestors() {
        if dir.join("plugins").is_dir() {
            return Some(dir.to_path_buf());
        }
        if best_git.is_none() && dir.join(".git").exists() {
            best_git = Some(dir.to_path_buf());
        }
    }
    best_git
}

/// Canonicalize `path`, mapping a failure to a clear "no such directory" error.
pub fn canonicalize_dir(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("no such directory: {path:?}"))
}

/// Canonicalize `path`, falling back to `path` unchanged if it doesn't exist
/// or canonicalization otherwise fails. For call sites that tolerate a
/// missing directory rather than surfacing an error.
pub fn canonicalize_or_self(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn finds_repo_root_by_plugins_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir_all(root.join("plugins").join("x")).unwrap();
        let deep = root.join("plugins").join("x");
        assert_eq!(find_repo_root(&deep).unwrap(), root);
    }

    #[test]
    fn falls_back_to_git_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        let sub = root.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(find_repo_root(&sub).unwrap(), root);
    }

    #[test]
    fn prefers_plugins_over_git() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        let inner = root.join("nested");
        fs::create_dir_all(inner.join("plugins")).unwrap();
        // Starting inside `nested`, the plugins dir is the closer match.
        assert_eq!(find_repo_root(&inner).unwrap(), inner);
    }

    #[test]
    fn returns_none_when_nothing_found() {
        let tmp = TempDir::new().unwrap();
        assert!(find_repo_root(tmp.path()).is_none());
    }

    #[test]
    fn config_dir_ends_with_aip_cli() {
        assert!(config_dir().ends_with("aip-cli"));
    }

    #[test]
    fn canonicalize_dir_returns_canonical_path_for_existing_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("existing");
        fs::create_dir_all(&dir).unwrap();
        let got = canonicalize_dir(&dir).unwrap();
        assert_eq!(got, dir.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_dir_surfaces_fixed_message_on_missing_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let err = canonicalize_dir(&missing).unwrap_err();
        assert!(err
            .to_string()
            .contains(&format!("no such directory: {missing:?}")));
    }

    #[test]
    fn canonicalize_or_self_returns_input_unchanged_on_missing_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let got = canonicalize_or_self(missing.clone());
        assert_eq!(got, missing);
    }

    #[test]
    fn canonicalize_dir_succeeds_for_existing_file_not_dir() {
        // `canonicalize_dir` delegates to `Path::canonicalize`, which resolves
        // any existing path regardless of whether it's a file or a directory.
        // Despite the "dir" in its name, it does NOT reject files.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("existing.txt");
        fs::write(&file, b"contents").unwrap();
        let got = canonicalize_dir(&file).unwrap();
        assert_eq!(got, file.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_or_self_canonicalizes_existing_dir() {
        // The tolerant-fallback branch (missing path) is covered above; this
        // locks the happy branch so a regression that always echoed the input
        // unchanged (defeating the point of canonicalization) would fail.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("existing");
        fs::create_dir_all(&dir).unwrap();
        let got = canonicalize_or_self(dir.clone());
        assert_eq!(got, dir.canonicalize().unwrap());
    }

    #[test]
    fn canonicalize_dir_resolves_dot_components_to_canonical_absolute_path() {
        // Proves the function actually resolves the filesystem (not a
        // passthrough) via `.`/`..` components, without mutating process cwd
        // (flaky under parallel tests) or asserting a hardcoded absolute
        // path string (fragile on macOS's /private symlink).
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("nested");
        fs::create_dir_all(&dir).unwrap();
        let messy = tmp.path().join(".").join("nested").join("..").join("nested");
        let got = canonicalize_dir(&messy).unwrap();
        assert!(got.is_absolute());
        assert_eq!(got, dir.canonicalize().unwrap());
    }
}
