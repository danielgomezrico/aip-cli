//! Per-host latest-model aliases for plugin agent frontmatter.
//!
//! Shared store files stay Claude-native (`opus` / `sonnet` / `haiku` / `fable`).
//! Codex and Pi cannot share those names, so setup stages a host-local copy and
//! rewrites `model:` to that host's family alias (tracks the latest in-family
//! model). `inherit` is left alone.

use crate::store::copy_dir_all;
use std::path::{Path, PathBuf};

/// Host whose plugin agents need a model rewrite at install time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Claude,
    Codex,
    Pi,
}

/// Latest-in-family alias for `host`.
///
/// Claude: family names (`opus`, `sonnet`, `haiku`, `fable`) already track the
/// newest model. Codex: `gpt-5.6` is the documented family alias — OpenAI
/// routes it to the flagship tier (`gpt-5.6-sol` today), so it follows the
/// family forward and must never be pinned to a tier suffix. `terra` / `luna`
/// have no alias of their own, so those two are named directly. Pi: glob
/// `*opus*` / `*sonnet*` / `*haiku*` / `*fable*` matches whatever the user's
/// providers currently expose.
pub fn latest_alias(host: Host, model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("inherit") {
        return trimmed.to_string();
    }
    match host {
        Host::Claude => claude_alias(trimmed),
        Host::Codex => codex_alias(trimmed),
        Host::Pi => pi_alias(trimmed),
    }
}

fn family(model: &str) -> Option<&'static str> {
    let lower = model.to_ascii_lowercase();
    // Longer tokens first so `claude-opus-4` is opus, not a later false match.
    for (needle, fam) in [
        ("opus", "opus"),
        ("sonnet", "sonnet"),
        ("haiku", "haiku"),
        ("fable", "fable"),
        ("gpt-5.6", "gpt-5.6"),
        ("gpt-5", "gpt-5.6"),
        ("o3", "gpt-5.6"),
        ("o4", "gpt-5.6"),
    ] {
        if lower.contains(needle) {
            return Some(fam);
        }
    }
    None
}

fn claude_alias(model: &str) -> String {
    match family(model) {
        Some("opus") => "opus".into(),
        Some("sonnet") => "sonnet".into(),
        Some("haiku") => "haiku".into(),
        Some("fable") => "fable".into(),
        _ => model.to_string(),
    }
}

fn codex_alias(model: &str) -> String {
    match family(model) {
        Some("opus") | Some("fable") | Some("gpt-5.6") => "gpt-5.6".into(),
        Some("sonnet") => "gpt-5.6-terra".into(),
        Some("haiku") => "gpt-5.6-luna".into(),
        _ => "gpt-5.6".into(),
    }
}

fn pi_alias(model: &str) -> String {
    match family(model) {
        Some("opus") => "*opus*".into(),
        Some("sonnet") => "*sonnet*".into(),
        Some("haiku") => "*haiku*".into(),
        Some("fable") => "*fable*".into(),
        Some("gpt-5.6") => "*gpt-5*".into(),
        _ => model.to_string(),
    }
}

/// Rewrite a YAML `model:` line (leading whitespace preserved). `None` if the
/// line is not a `model:` assignment.
pub fn rewrite_model_line(line: &str, host: Host) -> Option<String> {
    let (indent, rest) = split_indent(line);
    let rest = rest.strip_suffix('\r').unwrap_or(rest);
    let value = rest.strip_prefix("model:")?;
    if value.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        // `model:foo` without a separator is not YAML we authored.
        return None;
    }
    let raw = value.trim();
    if raw.is_empty() {
        return None;
    }
    let (quote, inner) = unquote(raw);
    let mapped = latest_alias(host, inner);
    let rendered = match quote {
        Some(q) => format!("{q}{mapped}{q}"),
        None => mapped,
    };
    Some(format!("{indent}model: {rendered}"))
}

fn split_indent(line: &str) -> (&str, &str) {
    let n = line.len() - line.trim_start_matches([' ', '\t']).len();
    (&line[..n], &line[n..])
}

fn unquote(raw: &str) -> (Option<char>, &str) {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0] as char;
        let last = bytes[bytes.len() - 1] as char;
        if (first == '"' || first == '\'') && first == last {
            return (Some(first), &raw[1..raw.len() - 1]);
        }
    }
    (None, raw)
}

/// Rewrite every `model:` line in `text`. Returns `None` when nothing changed.
pub fn rewrite_frontmatter(text: &str, host: Host) -> Option<String> {
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    let ends_nl = text.ends_with('\n');
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(rewritten) = rewrite_model_line(line, host) {
            if rewritten != line {
                changed = true;
            }
            out.push_str(&rewritten);
        } else {
            out.push_str(line);
        }
    }
    if ends_nl {
        out.push('\n');
    }
    changed.then_some(out)
}

/// Recursively rewrite `model:` in every `agents/*.md` under `root`.
/// Best-effort: unreadable files are skipped.
pub fn rewrite_agents_dir(root: &Path, host: Host) {
    let agents = root.join("agents");
    let Ok(rd) = std::fs::read_dir(&agents) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(rewritten) = rewrite_frontmatter(&text, host) {
            let _ = std::fs::write(&path, rewritten);
        }
    }
}

/// Host-local staged copy of a store plugin: `<store>/.hosts/<host>/<dir_name>`.
pub fn host_stage_dir(store_root: &Path, host: Host, dir_name: &str) -> PathBuf {
    let host_dir = match host {
        Host::Claude => "claude",
        Host::Codex => "codex",
        Host::Pi => "pi",
    };
    store_root.join(".hosts").join(host_dir).join(dir_name)
}

/// Copy `src` into the host stage and rewrite agent models. Returns the stage
/// path. `None` when the copy fails (caller should fall back to `src`).
pub fn stage_for_host(store_root: &Path, src: &Path, dir_name: &str, host: Host) -> Option<PathBuf> {
    let dest = host_stage_dir(store_root, host, dir_name);
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    copy_dir_all(src, &dest).ok()?;
    rewrite_agents_dir(&dest, host);
    Some(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn claude_keeps_family_aliases() {
        assert_eq!(latest_alias(Host::Claude, "opus"), "opus");
        assert_eq!(latest_alias(Host::Claude, "claude-opus-4-8"), "opus");
        assert_eq!(latest_alias(Host::Claude, "sonnet"), "sonnet");
        assert_eq!(latest_alias(Host::Claude, "inherit"), "inherit");
    }

    #[test]
    fn codex_maps_quality_first_families_to_the_gpt56_alias_not_a_pinned_tier() {
        for model in ["opus", "claude-opus-4-8", "fable", "claude-fable-5"] {
            assert_eq!(latest_alias(Host::Codex, model), "gpt-5.6");
        }
    }

    #[test]
    fn codex_never_pins_a_tier_suffix_that_a_family_update_could_move() {
        for model in ["opus", "fable", "gpt-5.6", "o3", "mystery"] {
            assert!(!latest_alias(Host::Codex, model).contains("-sol"));
        }
    }

    #[test]
    fn codex_names_terra_and_luna_directly_because_neither_has_an_alias() {
        assert_eq!(latest_alias(Host::Codex, "sonnet"), "gpt-5.6-terra");
        assert_eq!(latest_alias(Host::Codex, "haiku"), "gpt-5.6-luna");
    }

    #[test]
    fn codex_falls_back_to_the_family_alias_for_unknown_models() {
        assert_eq!(latest_alias(Host::Codex, "mystery-model"), "gpt-5.6");
    }

    #[test]
    fn codex_leaves_inherit_alone() {
        assert_eq!(latest_alias(Host::Codex, "inherit"), "inherit");
    }

    #[test]
    fn pi_uses_globs() {
        assert_eq!(latest_alias(Host::Pi, "opus"), "*opus*");
        assert_eq!(latest_alias(Host::Pi, "sonnet"), "*sonnet*");
        assert_eq!(latest_alias(Host::Pi, "claude-sonnet-4"), "*sonnet*");
        assert_eq!(latest_alias(Host::Pi, "inherit"), "inherit");
    }

    #[test]
    fn rewrite_preserves_indent_and_quotes() {
        assert_eq!(
            rewrite_model_line("model: opus", Host::Codex).as_deref(),
            Some("model: gpt-5.6")
        );
        assert_eq!(
            rewrite_model_line("  model: \"sonnet\"", Host::Pi).as_deref(),
            Some("  model: \"*sonnet*\"")
        );
        assert_eq!(rewrite_model_line("color: blue", Host::Codex), None);
    }

    #[test]
    fn rewrite_frontmatter_only_when_changed() {
        let src = "---\nname: x\nmodel: opus\n---\nbody\n";
        let out = rewrite_frontmatter(src, Host::Codex).unwrap();
        assert!(out.contains("model: gpt-5.6"));
        assert!(rewrite_frontmatter(src, Host::Claude).is_none());
    }

    #[test]
    fn stage_rewrites_agents_not_source() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("agents")).unwrap();
        std::fs::write(
            src.join("agents").join("lead.md"),
            "---\nmodel: opus\n---\n# Lead\n",
        )
        .unwrap();
        let dest = stage_for_host(tmp.path(), &src, "plug", Host::Codex).unwrap();
        let staged = std::fs::read_to_string(dest.join("agents").join("lead.md")).unwrap();
        let original = std::fs::read_to_string(src.join("agents").join("lead.md")).unwrap();
        assert!(staged.contains("model: gpt-5.6"));
        assert!(original.contains("model: opus"));
        assert!(!original.contains("gpt-5.6"));
    }
}
