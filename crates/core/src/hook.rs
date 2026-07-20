//! The `.aip-cli.toml` marker, a direnv-style trust store, and pure decision
//! helpers kept for doctor/compat. Auto-on-cd is disabled; use `aip-cli enable`.

use crate::config::{config_dir, MARKER_NAME};
use crate::mode_apply::Target;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Contents of a `.aip-cli.toml` marker file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Marker {
    /// Mode selector to apply (e.g. `"mobile"` or `"frontend product"`).
    pub mode: String,
    /// Optional single agent to restrict to. When omitted (the default), the
    /// mode is applied to *every* installed agent, so the user never has to
    /// pick one.
    #[serde(default)]
    pub target: Option<MarkerTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MarkerTarget {
    #[default]
    Claude,
    Grok,
    Pi,
}

impl From<MarkerTarget> for Target {
    fn from(t: MarkerTarget) -> Self {
        match t {
            MarkerTarget::Claude => Target::ClaudeCode,
            MarkerTarget::Grok => Target::Grok,
            MarkerTarget::Pi => Target::Pi,
        }
    }
}

impl Marker {
    pub fn parse(text: &str) -> Result<Marker, toml::de::Error> {
        toml::from_str(text)
    }

    /// Render a marker file body. With `target = None` (the common case) no
    /// `target` line is written, so the mode applies to every installed agent.
    /// Always parses back to an equal `Marker`.
    pub fn render(mode: &str, target: Option<MarkerTarget>) -> String {
        let mut out = format!("mode = {}\n", toml_basic_string(mode));
        match target {
            Some(MarkerTarget::Grok) => out.push_str("target = \"grok\"\n"),
            Some(MarkerTarget::Claude) => out.push_str("target = \"claude\"\n"),
            Some(MarkerTarget::Pi) => out.push_str("target = \"pi\"\n"),
            None => {}
        }
        out
    }
}

/// Quote `s` as a TOML basic (double-quoted) string, escaping the characters
/// TOML requires so the result always parses back to exactly `s`.
fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Hex SHA-256 of a marker's content — the trust + change-detection key.
pub fn content_hash(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Find the nearest ancestor of `start` (inclusive) containing a marker file.
pub fn find_marker(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(MARKER_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ─── trust store ────────────────────────────────────────────────────────────

/// A direnv-style allow-list: a marker is trusted only while its content hash
/// matches the recorded one, so editing a marker requires re-allowing it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TrustStore {
    /// Map of absolute marker path -> allowed content hash.
    entries: Vec<(String, String)>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the on-disk format: one `"<hash> <path>"` per line.
    pub fn parse(text: &str) -> TrustStore {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((hash, path)) = line.split_once(char::is_whitespace) {
                entries.push((path.trim().to_string(), hash.trim().to_string()));
            }
        }
        TrustStore { entries }
    }

    /// Serialize back to the on-disk format.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for (path, hash) in &self.entries {
            out.push_str(hash);
            out.push(' ');
            out.push_str(path);
            out.push('\n');
        }
        out
    }

    /// Record (or update) trust for `path` at `hash`.
    pub fn allow(&mut self, path: &str, hash: &str) {
        if let Some(e) = self.entries.iter_mut().find(|(p, _)| p == path) {
            e.1 = hash.to_string();
        } else {
            self.entries.push((path.to_string(), hash.to_string()));
        }
    }

    /// Remove trust for `path` (returns whether anything was removed).
    pub fn deny(&mut self, path: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(p, _)| p != path);
        self.entries.len() != before
    }

    /// True when `path` is trusted at exactly `hash`.
    pub fn is_allowed(&self, path: &str, hash: &str) -> bool {
        self.entries.iter().any(|(p, h)| p == path && h == hash)
    }

    fn store_path() -> PathBuf {
        config_dir().join("trust")
    }

    /// Load the store from `<config_dir>/trust` (empty if absent).
    pub fn load() -> TrustStore {
        match std::fs::read_to_string(Self::store_path()) {
            Ok(text) => TrustStore::parse(&text),
            Err(_) => TrustStore::new(),
        }
    }

    /// Persist the store, creating the config dir if needed.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.serialize())
    }
}

// ─── marker decision helpers (pure; used by doctor / tests) ──────────────────

/// What a legacy auto-apply path would do for the current directory.
/// Kept for unit tests and doctor-style analysis; shell auto-on-cd is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoAction {
    /// Apply `mode`; `marker` is the absolute marker path. `target` is `Some`
    /// only when the marker restricts to one agent — otherwise apply to all.
    Activate {
        marker: PathBuf,
        mode: String,
        target: Option<Target>,
    },
    /// A trusted marker whose mode is already applied — do nothing.
    AlreadyActive,
    /// A marker exists but is not trusted; tell the user to allow it.
    Untrusted { marker: PathBuf },
    /// A marker exists but is malformed.
    Invalid { marker: PathBuf, error: String },
    /// No marker in scope — nothing to do.
    None,
}

/// Decide what an auto-apply would do, purely from inputs (no filesystem).
///
/// * `marker` — `(path, raw_contents)` when a marker was found.
/// * `trusted` — whether the trust store allows this marker at its current hash.
/// * `applied_hash` — the content hash of the marker last applied, if any.
pub fn decide(
    marker: Option<(PathBuf, String)>,
    trusted: bool,
    applied_hash: Option<&str>,
) -> AutoAction {
    let (path, text) = match marker {
        Some(m) => m,
        None => return AutoAction::None,
    };
    if !trusted {
        return AutoAction::Untrusted { marker: path };
    }
    let parsed = match Marker::parse(&text) {
        Ok(m) => m,
        Err(e) => {
            return AutoAction::Invalid {
                marker: path,
                error: e.to_string(),
            }
        }
    };
    let hash = content_hash(&text);
    if applied_hash == Some(hash.as_str()) {
        return AutoAction::AlreadyActive;
    }
    AutoAction::Activate {
        marker: path,
        mode: parsed.mode,
        target: parsed.target.map(Into::into),
    }
}

// ─── applied state ───────────────────────────────────────────────────────────

fn applied_path() -> PathBuf {
    config_dir().join("applied")
}

/// The content hash of the marker last applied, if any.
pub fn load_applied_hash() -> Option<String> {
    std::fs::read_to_string(applied_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Record `hash` as the last applied marker.
pub fn save_applied_hash(hash: &str) -> std::io::Result<()> {
    let path = applied_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn marker_parse_defaults_target_to_none() {
        let m = Marker::parse(r#"mode = "mobile""#).unwrap();
        assert_eq!(m.mode, "mobile");
        assert_eq!(m.target, None); // None == apply to all agents
    }

    #[test]
    fn marker_parse_grok_target() {
        let m = Marker::parse("mode = \"frontend\"\ntarget = \"grok\"").unwrap();
        assert_eq!(m.target, Some(MarkerTarget::Grok));
        assert_eq!(Target::from(m.target.unwrap()), Target::Grok);
    }

    #[test]
    fn marker_requires_mode() {
        assert!(Marker::parse(r#"target = "grok""#).is_err());
    }

    #[test]
    fn marker_render_default_omits_target() {
        let body = Marker::render("mobile", None);
        assert_eq!(body, "mode = \"mobile\"\n");
        let parsed = Marker::parse(&body).unwrap();
        assert_eq!(parsed.mode, "mobile");
        assert_eq!(parsed.target, None);
    }

    #[test]
    fn marker_render_grok_includes_target_and_roundtrips() {
        let body = Marker::render("frontend product", Some(MarkerTarget::Grok));
        let parsed = Marker::parse(&body).unwrap();
        assert_eq!(parsed.mode, "frontend product");
        assert_eq!(parsed.target, Some(MarkerTarget::Grok));
    }

    #[test]
    fn marker_render_escapes_special_chars_and_roundtrips() {
        // render() documents "Always parses back to an equal Marker". A mode
        // carrying a quote, backslash, or control char must still produce valid
        // TOML that round-trips to the exact same string.
        for mode in [
            "a\"b",
            "back\\slash",
            "q\"and\\slash",
            "tab\there",
            "nl\nhere",
        ] {
            let body = Marker::render(mode, Some(MarkerTarget::Grok));
            let parsed = Marker::parse(&body)
                .unwrap_or_else(|e| panic!("render({mode:?}) produced invalid TOML: {e}\n{body}"));
            assert_eq!(parsed.mode, mode, "mode did not round-trip");
            assert_eq!(parsed.target, Some(MarkerTarget::Grok));
        }
    }

    #[test]
    fn content_hash_is_stable_and_distinct() {
        assert_eq!(content_hash("a"), content_hash("a"));
        assert_ne!(content_hash("a"), content_hash("b"));
        assert_eq!(content_hash("a").len(), 64);
    }

    #[test]
    fn find_marker_walks_up() {
        let tmp = TempDir::new().unwrap();
        let deep = tmp.path().join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        let marker = tmp.path().join("a").join(MARKER_NAME);
        fs::write(&marker, "mode=\"x\"").unwrap();
        assert_eq!(find_marker(&deep).unwrap(), marker);
    }

    #[test]
    fn find_marker_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        assert!(find_marker(tmp.path()).is_none());
    }

    #[test]
    fn trust_store_roundtrip() {
        let mut s = TrustStore::new();
        s.allow("/repo/.aip-cli.toml", "abc");
        assert!(s.is_allowed("/repo/.aip-cli.toml", "abc"));
        assert!(!s.is_allowed("/repo/.aip-cli.toml", "def"));
        let text = s.serialize();
        let parsed = TrustStore::parse(&text);
        assert_eq!(parsed, s);
    }

    #[test]
    fn trust_store_allow_updates_hash() {
        let mut s = TrustStore::new();
        s.allow("/m", "h1");
        s.allow("/m", "h2");
        assert!(!s.is_allowed("/m", "h1"));
        assert!(s.is_allowed("/m", "h2"));
        assert_eq!(s.serialize().lines().count(), 1);
    }

    #[test]
    fn trust_store_deny() {
        let mut s = TrustStore::new();
        s.allow("/m", "h");
        assert!(s.deny("/m"));
        assert!(!s.deny("/m"));
        assert!(!s.is_allowed("/m", "h"));
    }

    #[test]
    fn trust_store_parse_ignores_comments_and_blanks() {
        let s = TrustStore::parse("# header\n\nh1 /a\nh2 /b\n");
        assert!(s.is_allowed("/a", "h1"));
        assert!(s.is_allowed("/b", "h2"));
    }

    #[test]
    fn decide_none_without_marker() {
        assert_eq!(decide(None, false, None), AutoAction::None);
    }

    #[test]
    fn decide_untrusted_marker() {
        let m = (PathBuf::from("/m"), "mode=\"x\"".to_string());
        assert_eq!(
            decide(Some(m), false, None),
            AutoAction::Untrusted {
                marker: PathBuf::from("/m")
            }
        );
    }

    #[test]
    fn decide_activate_when_trusted_and_new() {
        let text = "mode = \"mobile\"".to_string();
        let m = (PathBuf::from("/m"), text.clone());
        match decide(Some(m), true, None) {
            AutoAction::Activate { mode, target, .. } => {
                assert_eq!(mode, "mobile");
                assert_eq!(target, None); // no target line => all agents
            }
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn decide_already_active_when_hash_matches() {
        let text = "mode = \"mobile\"".to_string();
        let hash = content_hash(&text);
        let m = (PathBuf::from("/m"), text);
        assert_eq!(
            decide(Some(m), true, Some(&hash)),
            AutoAction::AlreadyActive
        );
    }

    #[test]
    fn decide_invalid_marker() {
        let m = (PathBuf::from("/m"), "not = valid = toml".to_string());
        match decide(Some(m), true, None) {
            AutoAction::Invalid { .. } => {}
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
