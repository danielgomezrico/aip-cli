//! Per-host latest-model aliases for plugin agent frontmatter.
//!
//! Shared store files stay Claude-native (`opus` / `sonnet` / `haiku` / `fable`).
//! Codex and Pi cannot share those names, so setup stages a host-local copy and
//! rewrites `model:` to that host's family alias (tracks the latest in-family
//! model). `inherit` is left alone.

use crate::store::copy_dir_all;
use std::collections::BTreeSet;
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PiModelCatalog {
    models: BTreeSet<String>,
}

impl PiModelCatalog {
    pub(crate) fn detect() -> Self {
        std::process::Command::new("pi")
            .args(pi_model_list_args())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| Self::parse(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default()
    }

    pub(crate) fn parse(output: &str) -> Self {
        let models = output
            .lines()
            .filter_map(|line| {
                let mut columns = line.split_whitespace();
                let provider = columns.next()?;
                let model = columns.next()?;
                (provider != "provider").then(|| format!("{provider}/{model}"))
            })
            .collect();
        Self { models }
    }

    fn first_available<'a>(&self, preferred: &[&'a str]) -> Option<&'a str> {
        preferred
            .iter()
            .copied()
            .find(|model| self.models.contains(*model))
    }
}

fn pi_model_list_args() -> [&'static str; 6] {
    [
        "--no-skills",
        "--no-extensions",
        "--no-prompt-templates",
        "--no-themes",
        "--no-context-files",
        "--list-models",
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PiAgentRole {
    Thinking,
    Programming,
    Doing,
    Unknown,
}

fn pi_agent_role(text: &str) -> PiAgentRole {
    let frontmatter = text
        .split("---")
        .nth(1)
        .unwrap_or(text)
        .to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| frontmatter.contains(word));

    if has(&[
        "architect",
        "review",
        "critic",
        "analyst",
        "investigat",
        "research",
        "plan",
        "design",
        "audit",
        "diagnos",
        "advisor",
        "strateg",
        "brainstorm",
        "grade",
        "scope",
    ]) {
        PiAgentRole::Thinking
    } else if has(&[
        "setup",
        "install",
        "ship",
        "release",
        "static analysis",
        "format",
        "lint",
        "fetch",
        "gather",
        "capture",
        "run",
        "sync",
        "update",
        "migrate",
        "convert",
        "generate",
    ]) {
        PiAgentRole::Doing
    } else if has(&[
        "implement",
        "developer",
        "engineer",
        "code",
        "coding",
        "refactor",
        "tdd",
        "test",
        "debug",
        "fix",
        "build",
        "program",
    ]) {
        PiAgentRole::Programming
    } else {
        PiAgentRole::Unknown
    }
}

fn is_mapped_family(model: &str) -> bool {
    ["opus", "sonnet", "haiku", "fable", "gpt-5"]
        .iter()
        .any(|family| model.to_ascii_lowercase().contains(family))
}

fn pi_agent_model<'a>(
    catalog: &'a PiModelCatalog,
    role: PiAgentRole,
    current: &str,
) -> Option<&'a str> {
    let current = current.trim_matches(['\'', '"']);
    if current.eq_ignore_ascii_case("inherit")
        || (!current.is_empty() && !is_mapped_family(current))
    {
        return None;
    }

    let high_reasoning = current.contains("opus") || current.contains("fable");
    match role {
        PiAgentRole::Thinking if high_reasoning => {
            catalog.first_available(&["ollama/muse-glimmer", "ollama/qwen3.8-27b"])
        }
        PiAgentRole::Thinking => {
            catalog.first_available(&["ollama/qwen3.8-27b", "ollama/muse-glimmer"])
        }
        PiAgentRole::Programming => catalog.first_available(&[
            "ollama/qwen3-coder:30b",
            "ollama/qwen2.5-coder:32b",
            "ollama/qwen2.5-coder:7b",
        ]),
        PiAgentRole::Doing => catalog.first_available(&[
            "ollama/qwen2.5-coder:7b",
            "ollama/qwen3-coder:30b",
            "ollama/qwen2.5-coder:32b",
        ]),
        PiAgentRole::Unknown if high_reasoning => {
            catalog.first_available(&["ollama/muse-glimmer", "ollama/qwen3.8-27b"])
        }
        PiAgentRole::Unknown if current.contains("sonnet") => {
            catalog.first_available(&["ollama/qwen3.8-27b", "ollama/muse-glimmer"])
        }
        PiAgentRole::Unknown => {
            catalog.first_available(&["ollama/qwen2.5-coder:7b", "ollama/qwen3-coder:30b"])
        }
    }
}

fn rewrite_pi_agent(text: &str, catalog: &PiModelCatalog) -> Option<String> {
    let role = pi_agent_role(text);
    let mut changed = false;
    let mut in_frontmatter = false;
    let mut out = String::with_capacity(text.len());

    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim() == "---" {
            in_frontmatter = !in_frontmatter;
            out.push_str(line);
            continue;
        }
        let (indent, rest) = split_indent(line);
        let replacement = in_frontmatter
            .then(|| rest.strip_prefix("model:"))
            .flatten()
            .and_then(|value| pi_agent_model(catalog, role, value.trim()));
        if let Some(model) = replacement {
            out.push_str(&format!("{indent}model: {model}"));
            changed |= rest != format!("model: {model}");
        } else {
            out.push_str(line);
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    changed.then_some(out)
}

fn yaml_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\t' => quoted.push_str("\\t"),
            '\r' => quoted.push_str("\\r"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn pi_name_part(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
        } else if !normalized.ends_with('-') {
            normalized.push('-');
        }
    }
    normalized.trim_matches('-').to_string()
}

fn stable_name_hash(value: &str) -> u32 {
    value.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

fn pi_skill_name(plugin: &str, skill: &str) -> String {
    let plugin = pi_name_part(plugin);
    let skill = pi_name_part(skill);
    let plugin = if plugin.is_empty() { "plugin" } else { &plugin };
    let skill = if skill.is_empty() { "skill" } else { &skill };
    let scoped = if skill == plugin || skill.starts_with(&format!("{plugin}-")) {
        skill.to_string()
    } else {
        format!("{plugin}-{skill}")
    };
    if scoped.len() <= 64 {
        return scoped;
    }
    let suffix = format!("-{:08x}", stable_name_hash(&scoped));
    let keep = 64 - suffix.len();
    let mut prefix = scoped[..keep].trim_end_matches('-').to_string();
    prefix.push_str(&suffix);
    prefix
}

fn frontmatter_name(text: &str) -> Option<&str> {
    text.split("---")
        .nth(1)?
        .lines()
        .find_map(|line| line.trim().strip_prefix("name:"))
        .map(str::trim)
        .map(|name| name.trim_matches(['\'', '"']))
        .filter(|name| !name.is_empty())
}

fn normalize_pi_skill(text: &str, plugin_name: &str, default_skill_name: &str) -> Option<String> {
    let mut changed = false;
    let mut in_frontmatter = false;
    let mut out = String::with_capacity(text.len());
    let declared_name = frontmatter_name(text).unwrap_or(default_skill_name);
    let scoped_name = pi_skill_name(plugin_name, declared_name);
    let has_name = frontmatter_name(text).is_some();

    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim() == "---" {
            in_frontmatter = !in_frontmatter;
            out.push_str(line);
            if in_frontmatter && !has_name {
                out.push_str(&format!("\nname: {scoped_name}"));
                changed = true;
            }
            continue;
        }
        let (indent, rest) = split_indent(line);
        if in_frontmatter && rest.strip_prefix("name:").is_some() {
            let replacement = format!("{indent}name: {scoped_name}");
            changed |= replacement != line;
            out.push_str(&replacement);
            continue;
        }
        let value = in_frontmatter
            .then(|| rest.strip_prefix("description:"))
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.starts_with(['"', '\'', '|', '>']));
        if let Some(value) = value {
            out.push_str(&format!("{indent}description: {}", yaml_quote(value)));
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    changed.then_some(out)
}

fn rewrite_pi_skill_tree(root: &Path, plugin_name: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rewrite_pi_skill_tree(&path, plugin_name);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let default_skill_name = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("skill");
            if let Some(rewritten) = normalize_pi_skill(&text, plugin_name, default_skill_name) {
                let _ = std::fs::write(path, rewritten);
            }
        }
    }
}

pub(crate) fn prepare_pi_package(root: &Path, plugin_name: &str, catalog: &PiModelCatalog) {
    let agents = root.join("agents");
    if let Ok(entries) = std::fs::read_dir(agents) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(rewritten) = rewrite_pi_agent(&text, catalog) {
                let _ = std::fs::write(path, rewritten);
            }
        }
    }
    rewrite_pi_skill_tree(&root.join("skills"), plugin_name);
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
pub fn stage_for_host(
    store_root: &Path,
    src: &Path,
    dir_name: &str,
    host: Host,
) -> Option<PathBuf> {
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

    fn pi_catalog() -> PiModelCatalog {
        PiModelCatalog::parse(
            "provider model context max-out thinking images\n\
             ollama muse-glimmer 131K 8K yes yes\n\
             ollama qwen3.8-27b 131K 8K no no\n\
             ollama qwen3-coder:30b 16K 8K no no\n\
             ollama qwen2.5-coder:7b 16K 8K no no\n",
        )
    }

    #[test]
    fn pi_model_listing_disables_all_project_resources() {
        assert_eq!(
            pi_model_list_args(),
            [
                "--no-skills",
                "--no-extensions",
                "--no-prompt-templates",
                "--no-themes",
                "--no-context-files",
                "--list-models",
            ]
        );
    }

    #[test]
    fn pi_thinking_agents_prefer_muse_then_qwen38_by_reasoning_tier() {
        let high = "---\nname: plan-critic\ndescription: Reviews plans\nmodel: *opus*\n---\n";
        let normal =
            "---\nname: investigator\ndescription: Investigates facts\nmodel: *sonnet*\n---\n";
        assert!(rewrite_pi_agent(high, &pi_catalog())
            .unwrap()
            .contains("model: ollama/muse-glimmer"));
        assert!(rewrite_pi_agent(normal, &pi_catalog())
            .unwrap()
            .contains("model: ollama/qwen3.8-27b"));
    }

    #[test]
    fn pi_programming_and_doing_agents_use_available_code_models() {
        let programming =
            "---\nname: rust-developer\ndescription: Implements Rust code\nmodel: *sonnet*\n---\n";
        let doing = "---\nname: static-analysis\ndescription: Runs lint\nmodel: *haiku*\n---\n";
        assert!(rewrite_pi_agent(programming, &pi_catalog())
            .unwrap()
            .contains("model: ollama/qwen3-coder:30b"));
        assert!(rewrite_pi_agent(doing, &pi_catalog())
            .unwrap()
            .contains("model: ollama/qwen2.5-coder:7b"));
    }

    #[test]
    fn pi_keeps_existing_alias_when_preferred_models_are_unavailable() {
        let agent = "---\nname: plan-critic\ndescription: Reviews plans\nmodel: *opus*\n---\n";
        assert_eq!(rewrite_pi_agent(agent, &PiModelCatalog::default()), None);
    }

    #[test]
    fn pi_skill_descriptions_are_quoted_without_changing_the_source() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let staged = tmp.path().join("staged");
        std::fs::create_dir_all(source.join("skills").join("nested")).unwrap();
        let skill = source.join("skills").join("nested").join("SKILL.md");
        let text =
            "---\nname: nested\ndescription: Use when task: details include colon\n---\n# Skill\n";
        std::fs::write(&skill, text).unwrap();
        copy_dir_all(&source, &staged).unwrap();
        prepare_pi_package(&staged, "investigation", &pi_catalog());
        let rewritten = std::fs::read_to_string(staged.join("skills/nested/SKILL.md")).unwrap();
        assert!(rewritten.contains("name: investigation-nested"));
        assert!(rewritten.contains("description: \"Use when task: details include colon\""));
        assert_eq!(std::fs::read_to_string(skill).unwrap(), text);
    }

    #[test]
    fn pi_scopes_same_named_skills_to_every_plugin() {
        let skill = "---\nname: code-critic\ndescription: Reviews code\n---\n";
        let flutter = normalize_pi_skill(skill, "flutter", "code-critic").unwrap();
        let frontend = normalize_pi_skill(skill, "frontend", "code-critic").unwrap();
        let native = normalize_pi_skill(skill, "frontend-native", "code-critic").unwrap();
        assert!(flutter.contains("name: flutter-code-critic"));
        assert!(frontend.contains("name: frontend-code-critic"));
        assert!(native.contains("name: frontend-native-code-critic"));
    }

    #[test]
    fn pi_scopes_skills_without_declared_names() {
        let skill = "---\ndescription: Reviews code\n---\n";
        let rewritten = normalize_pi_skill(skill, "frontend", "code-critic").unwrap();
        assert!(rewritten.starts_with("---\nname: frontend-code-critic\n"));
    }

    #[test]
    fn pi_scoped_skill_names_stay_stable_and_within_the_standard_limit() {
        let plugin = "very-long-plugin-name-that-consumes-most-of-the-name-budget";
        let skill = "another-very-long-skill-name-that-would-overflow-the-limit";
        let first = pi_skill_name(plugin, skill);
        assert_eq!(first, pi_skill_name(plugin, skill));
        assert!(first.len() <= 64);
        assert!(first.starts_with("very-long-plugin"));
    }
}
