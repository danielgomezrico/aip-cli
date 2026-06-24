//! The canonical `.aip-cli` plugin store.
//!
//! `aip-cli` keeps its own copy of every plugin under a single per-user
//! directory (`~/.aip-cli/plugins`). External plugins are *ingested* into this
//! store (see [`crate::ingest`]); once there, the store is the one location
//! `setup` and discovery read from, so the same plugin set is shared across
//! every AI agent on the machine.

use crate::manifest::PluginManifest;
use std::path::{Path, PathBuf};

/// Root of the per-user store: `~/.aip-cli` (falls back to `./.aip-cli` when the
/// home directory cannot be determined).
pub fn store_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".aip-cli")
}

/// Directory holding ingested plugins: `<store_dir>/plugins`.
pub fn plugins_dir() -> PathBuf {
    store_dir().join("plugins")
}

/// True when `dir` looks like a Claude plugin: it has a
/// `.claude-plugin/plugin.json` manifest.
pub fn is_plugin_dir(dir: &Path) -> bool {
    dir.join(".claude-plugin").join("plugin.json").is_file()
}

/// The directory name a plugin should occupy in the store: the source folder's
/// own name, preserving any layout its Makefile or scripts assume.
pub fn store_name(src: &Path) -> Option<String> {
    src.file_name().and_then(|s| s.to_str()).map(str::to_string)
}

/// Recursively copy `src` into `dest`, creating `dest` and any parents. The
/// `.git` directory is skipped so ingested checkouts don't carry VCS metadata.
///
/// Symlinks are recreated as symlinks rather than followed: this preserves a
/// plugin's on-disk layout and, crucially, avoids unbounded recursion (a stack
/// overflow) on a symlink that points back into an ancestor — `ingest-folder`
/// runs over arbitrary user-supplied directories.
pub fn copy_dir_all(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            copy_symlink(&from, &to)?;
        } else if file_type.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Recreate the symlink at `from` as a symlink at `to` with the same target.
#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(from)?;
    std::os::unix::fs::symlink(target, to)
}

/// Non-unix fallback: copy the symlink's resolved contents (best effort).
#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        copy_dir_all(from, to)
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// A naming collision in the store: the directory `slot` already holds plugin
/// `existing`, and we are being asked to install a *different* plugin `incoming`
/// in its place. Passed to the conflict callback of [`install_plugin_with`] so
/// the caller can decide (e.g. by prompting) whether to overwrite.
#[derive(Debug, Clone, Copy)]
pub struct StoreConflict<'a> {
    /// Destination directory inside the store that is already occupied.
    pub slot: &'a Path,
    /// Manifest name of the plugin currently in the slot.
    pub existing: &'a str,
    /// Manifest name of the plugin being installed.
    pub incoming: &'a str,
}

/// Copy the plugin at `src` into the store directory `plugins_root`, placing it
/// under its source folder name. An existing copy of the *same* plugin is
/// replaced; a *different* plugin occupying the same slot is refused. Returns
/// the destination path.
///
/// Errors if `src` is not a plugin directory.
pub fn install_plugin(src: &Path, plugins_root: &Path) -> std::io::Result<PathBuf> {
    // Default policy: never clobber a different plugin.
    install_plugin_with(src, plugins_root, &mut |_| false)
}

/// Like [`install_plugin`], but on a different-plugin slot collision it consults
/// `on_conflict`. Returning `true` overwrites the slot; `false` refuses with an
/// `AlreadyExists` error. Replacing the *same* plugin never invokes the callback.
pub fn install_plugin_with(
    src: &Path,
    plugins_root: &Path,
    on_conflict: &mut dyn FnMut(&StoreConflict) -> bool,
) -> std::io::Result<PathBuf> {
    if !is_plugin_dir(src) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is not a plugin (no .claude-plugin/plugin.json)",
                src.display()
            ),
        ));
    }
    let name = store_name(src).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin source has no directory name",
        )
    })?;
    let dest = plugins_root.join(&name);
    if dest.exists() {
        // Replacing the same plugin is fine; a *different* plugin sharing the
        // directory name is only overwritten when `on_conflict` approves it.
        if let (Ok(existing), Ok(incoming)) =
            (PluginManifest::read(&dest), PluginManifest::read(src))
        {
            if existing.name != incoming.name {
                let conflict = StoreConflict {
                    slot: &dest,
                    existing: &existing.name,
                    incoming: &incoming.name,
                };
                if !on_conflict(&conflict) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "store slot {} already holds plugin '{}'; refusing to overwrite with '{}'",
                            dest.display(),
                            existing.name,
                            incoming.name
                        ),
                    ));
                }
            }
        }
        std::fs::remove_dir_all(&dest)?;
    }
    copy_dir_all(src, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Build a minimal but valid plugin tree at `dir`.
    fn make_plugin(dir: &Path, name: &str) {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
        )
        .unwrap();
        fs::write(dir.join("README.md"), "hello").unwrap();
    }

    #[test]
    fn plugins_dir_ends_with_plugins() {
        assert!(plugins_dir().ends_with("plugins"));
        assert!(store_dir().ends_with(".aip-cli"));
    }

    #[test]
    fn is_plugin_dir_detects_manifest() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("p");
        assert!(!is_plugin_dir(&p));
        make_plugin(&p, "p");
        assert!(is_plugin_dir(&p));
    }

    #[test]
    fn copy_dir_all_recurses_and_skips_git() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("sub").join("b.txt"), "b").unwrap();
        fs::write(src.join(".git").join("config"), "x").unwrap();

        let dest = tmp.path().join("dest");
        copy_dir_all(&src, &dest).unwrap();

        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "a");
        assert_eq!(
            fs::read_to_string(dest.join("sub").join("b.txt")).unwrap(),
            "b"
        );
        assert!(!dest.join(".git").exists());
    }

    #[test]
    fn install_plugin_copies_under_source_name() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("android-native");
        make_plugin(&src, "android");
        let store = tmp.path().join("store");

        let dest = install_plugin(&src, &store).unwrap();
        assert_eq!(dest, store.join("android-native"));
        assert!(is_plugin_dir(&dest));
        assert_eq!(fs::read_to_string(dest.join("README.md")).unwrap(), "hello");
    }

    #[test]
    fn install_plugin_replaces_existing() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("p");
        make_plugin(&src, "p");
        let store = tmp.path().join("store");

        install_plugin(&src, &store).unwrap();
        // Add a stale file to the existing copy, then re-install.
        fs::write(store.join("p").join("stale.txt"), "old").unwrap();
        install_plugin(&src, &store).unwrap();
        assert!(!store.join("p").join("stale.txt").exists());
    }

    #[test]
    fn install_plugin_refuses_to_clobber_different_plugin() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        // First plugin "alpha" lands in slot "tools".
        let a = tmp.path().join("a").join("tools");
        make_plugin(&a, "alpha");
        install_plugin(&a, &store).unwrap();
        // A *different* plugin "beta" with the same dir name must not overwrite.
        let b = tmp.path().join("b").join("tools");
        make_plugin(&b, "beta");
        let err = install_plugin(&b, &store).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        // The original survives.
        let kept = PluginManifest::read(&store.join("tools")).unwrap();
        assert_eq!(kept.name, "alpha");
    }

    #[test]
    fn install_plugin_with_overwrites_when_conflict_approved() {
        let tmp = TempDir::new().unwrap();
        let store = tmp.path().join("store");
        let a = tmp.path().join("a").join("tools");
        make_plugin(&a, "alpha");
        install_plugin(&a, &store).unwrap();
        // A different plugin in the same slot is overwritten when the callback
        // approves it, and the callback sees the real existing/incoming names.
        let b = tmp.path().join("b").join("tools");
        make_plugin(&b, "beta");
        let mut seen = None;
        install_plugin_with(&b, &store, &mut |c| {
            seen = Some((c.existing.to_string(), c.incoming.to_string()));
            true
        })
        .unwrap();
        assert_eq!(seen, Some(("alpha".to_string(), "beta".to_string())));
        let now = PluginManifest::read(&store.join("tools")).unwrap();
        assert_eq!(now.name, "beta");
    }

    #[test]
    #[cfg(unix)]
    fn copy_dir_all_preserves_symlinks_without_following() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("real.txt"), "data").unwrap();
        std::os::unix::fs::symlink("real.txt", src.join("link.txt")).unwrap();
        // A self-referential dir symlink would stack-overflow if followed.
        std::os::unix::fs::symlink("..", src.join("loop")).unwrap();

        let dest = tmp.path().join("dest");
        copy_dir_all(&src, &dest).unwrap();

        let meta = fs::symlink_metadata(dest.join("link.txt")).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(fs::read_link(dest.join("loop")).unwrap(), Path::new(".."));
    }

    #[test]
    fn install_plugin_rejects_non_plugin() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("not-a-plugin");
        fs::create_dir_all(&src).unwrap();
        let err = install_plugin(&src, &tmp.path().join("store")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
