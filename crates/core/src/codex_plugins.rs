//! Codex CLI plugin marketplace + install. Remove stays a single argv slot.

use crate::claude_plugins::resolve_marketplace_name;
use crate::host_models::{self, Host};
use crate::runner::{CommandRunner, Invocation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

/// `codex plugin remove <spec>` — uninstall one plugin. `spec` is a single
/// argv slot (do not split on `@`).
pub fn remove_invocation(spec: &str, cwd: &Path) -> Invocation {
    Invocation::new("codex", &["plugin", "remove", spec], cwd)
}

/// `codex plugin marketplace add <path>` — register a local marketplace root.
pub fn marketplace_add_invocation(path: &Path, cwd: &Path) -> Invocation {
    let p = path.to_string_lossy().into_owned();
    Invocation::new("codex", &["plugin", "marketplace", "add", &p], cwd)
}

/// `codex plugin marketplace remove <name>`.
pub fn marketplace_remove_invocation(name: &str, cwd: &Path) -> Invocation {
    Invocation::new("codex", &["plugin", "marketplace", "remove", name], cwd)
}

/// `codex plugin add <plugin>@<marketplace>`.
pub fn add_invocation(plugin: &str, marketplace: &str, cwd: &Path) -> Invocation {
    let spec = format!("{plugin}@{marketplace}");
    Invocation::new("codex", &["plugin", "add", &spec], cwd)
}

/// Marketplace name from `.codex-plugin/marketplace.json`, then Claude's
/// marketplace.json, then `dir_name`.
pub fn resolve_codex_marketplace_name(store_plugin_dir: &Path, dir_name: &str) -> String {
    let path = store_plugin_dir
        .join(".codex-plugin")
        .join("marketplace.json");
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some(name) = serde_json::from_str::<MarketplaceName>(&text)
            .ok()
            .and_then(|m| m.name)
            .filter(|s| !s.is_empty())
        {
            return name;
        }
    }
    resolve_marketplace_name(store_plugin_dir, dir_name)
}

#[derive(Deserialize)]
struct MarketplaceName {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct CodexConfig {
    #[serde(default)]
    marketplaces: BTreeMap<String, MarketplaceSource>,
}

#[derive(Deserialize, Default)]
struct MarketplaceSource {
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize)]
struct CodexAgentFile {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_reasoning_effort: Option<String>,
    developer_instructions: String,
}

fn yaml_scalar(value: &str) -> String {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0] as char;
        let last = bytes[bytes.len() - 1] as char;
        if (first == '"' || first == '\'') && first == last {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn frontmatter_value(lines: &[String]) -> String {
    let Some(first) = lines.first() else {
        return String::new();
    };
    let rest = lines[1..]
        .iter()
        .map(|line| line.trim().trim_start_matches("- "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    match first.trim() {
        "|" => rest.join("\n"),
        ">" => rest.join(" "),
        "" if !rest.is_empty() => rest.join(", "),
        value => yaml_scalar(value),
    }
}

fn parse_agent_document(text: &str) -> std::io::Result<(BTreeMap<String, String>, String)> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "agent is missing opening YAML frontmatter",
        ));
    }

    let mut raw = BTreeMap::<String, Vec<String>>::new();
    let mut current = None::<String>;
    let mut body = Vec::new();
    let mut closed = false;
    for line in &mut lines {
        if line.trim() == "---" {
            closed = true;
            body.extend(lines);
            break;
        }
        if !line.starts_with([' ', '\t']) {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                raw.insert(key.clone(), vec![value.trim().to_string()]);
                current = Some(key);
                continue;
            }
        }
        if let Some(key) = &current {
            raw.get_mut(key)
                .expect("current key exists")
                .push(line.to_string());
        }
    }
    if !closed {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "agent is missing closing YAML frontmatter",
        ));
    }

    let fields = raw
        .into_iter()
        .map(|(key, value)| (key, frontmatter_value(&value)))
        .collect();
    Ok((fields, body.join("\n").trim().to_string()))
}

fn compatibility_instructions(fields: &BTreeMap<String, String>) -> Vec<String> {
    fields
        .iter()
        .filter_map(|(key, value)| {
            if value.is_empty() {
                return None;
            }
            match key.as_str() {
                "name" | "description" | "model" | "effort" | "color" => None,
                "skills" => Some(format!("Use these skills when applicable: {value}.")),
                "tools" => Some(format!(
                    "Prefer Codex equivalents of these source tools when available: {value}."
                )),
                "disallowedTools" | "disallowed_tools" => Some(format!(
                    "Do not use Codex equivalents of these source tools: {value}."
                )),
                "maxTurns" | "max_turns" => {
                    Some(format!("Complete the task within at most {value} turns."))
                }
                "isolation" => Some(format!("Honor the source isolation mode: {value}.")),
                "permissionMode" | "permission_mode" => {
                    Some(format!("Honor the source permission mode: {value}."))
                }
                _ => Some(format!("Honor source agent metadata `{key}: {value}`.")),
            }
        })
        .collect()
}

fn render_codex_agent(text: &str) -> std::io::Result<String> {
    let (fields, body) = parse_agent_document(text)?;
    let required = |key: &str| {
        fields
            .get(key)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, format!("agent is missing {key}")))
    };
    let name = required("name")?;
    let description = required("description")?;
    if body.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "agent is missing developer instructions",
        ));
    }

    let model = fields.get("model").and_then(|model| {
        (!model.eq_ignore_ascii_case("inherit"))
            .then(|| host_models::latest_alias(Host::Codex, model))
    });
    let model_reasoning_effort = fields.get("effort").and_then(|effort| {
        matches!(
            effort.as_str(),
            "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        )
        .then(|| effort.clone())
    });
    let compatibility = compatibility_instructions(&fields);
    let developer_instructions = if compatibility.is_empty() {
        body
    } else {
        format!(
            "{body}\n\nClaude compatibility instructions:\n- {}",
            compatibility.join("\n- ")
        )
    };
    toml::to_string_pretty(&CodexAgentFile {
        name,
        description,
        model,
        model_reasoning_effort,
        developer_instructions,
    })
    .map_err(Error::other)
}

fn safe_name_part(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn stable_name_hash(value: &str) -> u32 {
    value.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    })
}

fn plugin_key(plugin: &str) -> String {
    let readable = safe_name_part(plugin);
    let readable = if readable.is_empty() {
        "plugin"
    } else {
        &readable
    };
    format!("{readable}-{:08x}", stable_name_hash(plugin))
}

fn ownership_marker(plugin: &str) -> String {
    format!("# aip-cli owner: {}\n", plugin_key(plugin))
}

fn generated_prefix(plugin: &str) -> String {
    format!("aip-{}-", plugin_key(plugin))
}

fn generated_agent_name(plugin: &str, relative: &Path) -> String {
    let relative = relative.to_string_lossy();
    let readable = safe_name_part(relative.trim_end_matches(".md"));
    let readable = if readable.is_empty() {
        "agent"
    } else {
        &readable
    };
    format!(
        "{}{readable}-{:08x}.toml",
        generated_prefix(plugin),
        stable_name_hash(&relative)
    )
}

fn collect_agent_markdown(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_agent_markdown(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

fn is_owned_agent(path: &Path, plugin: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&generated_prefix(plugin)))
        && std::fs::read_to_string(path)
            .is_ok_and(|text| text.starts_with(&ownership_marker(plugin)))
}

pub fn remove_registered_agents(home: &Path, plugin: &str) -> std::io::Result<usize> {
    let agents_dir = home.join(".codex").join("agents");
    let entries = match std::fs::read_dir(&agents_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let path = entry?.path();
        if is_owned_agent(&path, plugin) {
            std::fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn sync_registered_agents(home: &Path, plugin: &str, source: &Path) -> std::io::Result<usize> {
    let source_agents = source.join("agents");
    let mut files = Vec::new();
    collect_agent_markdown(&source_agents, &mut files)?;

    let marker = ownership_marker(plugin);
    let mut rendered = Vec::with_capacity(files.len());
    for path in files {
        let relative = path.strip_prefix(&source_agents).map_err(Error::other)?;
        let content = std::fs::read_to_string(&path)?;
        let toml = render_codex_agent(&content)?;
        rendered.push((
            generated_agent_name(plugin, relative),
            format!("{marker}{toml}"),
        ));
    }

    let agents_dir = home.join(".codex").join("agents");
    if !rendered.is_empty() {
        std::fs::create_dir_all(&agents_dir)?;
    }
    for (name, _) in &rendered {
        let target = agents_dir.join(name);
        if target.exists() && !is_owned_agent(&target, plugin) {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "refusing to overwrite unowned Codex agent {}",
                    target.display()
                ),
            ));
        }
    }

    remove_registered_agents(home, plugin)?;
    for (name, content) in &rendered {
        std::fs::write(agents_dir.join(name), content)?;
    }
    Ok(rendered.len())
}

fn read_codex_config(home: &Path) -> CodexConfig {
    let path = home.join(".codex").join("config.toml");
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => CodexConfig::default(),
    }
}

fn marketplace_source(home: &Path, marketplace: &str) -> Option<String> {
    read_codex_config(home)
        .marketplaces
        .get(marketplace)
        .and_then(|e| e.source.clone())
}

pub fn source_into_store(source: &str, store_root: &Path, dir_name: &str) -> bool {
    Path::new(source).starts_with(host_models::host_stage_dir(
        store_root,
        Host::Codex,
        dir_name,
    ))
}

pub fn sync_installed_plugin<R: CommandRunner + ?Sized>(
    runner: &R,
    home: &Path,
    store_root: &Path,
    dir_name: &str,
    manifest: &str,
    src: &Path,
    cwd: &Path,
) {
    let marketplace = resolve_codex_marketplace_name(src, dir_name);
    let install_root = host_models::stage_for_host(store_root, src, dir_name, Host::Codex)
        .unwrap_or_else(|| src.to_path_buf());
    if let Err(error) = sync_registered_agents(home, dir_name, src) {
        eprintln!("codex agents for {dir_name}: {error}");
    }

    let known = marketplace_source(home, &marketplace);
    let needs_add = match known.as_deref() {
        Some(loc) => !source_into_store(loc, store_root, dir_name),
        None => true,
    };
    if needs_add {
        if known.is_some() {
            let _ = runner.run(&marketplace_remove_invocation(&marketplace, cwd));
        }
        let _ = runner.run(&marketplace_add_invocation(&install_root, cwd));
    }

    let _ = runner.run(&add_invocation(manifest, &marketplace, cwd));
}

/// Used by tests that need a known stage path.
pub fn staged_plugin_path(store_root: &Path, dir_name: &str) -> PathBuf {
    host_models::host_stage_dir(store_root, Host::Codex, dir_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RecordingRunner;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_config(home: &Path, body: &str) {
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    fn write_plugin(dir: &Path, marketplace: &str, manifest: &str) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("marketplace.json"),
            format!(
                r#"{{"name":"{marketplace}","plugins":[{{"name":"{manifest}","source":"./"}}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("plugin.json"),
            format!(r#"{{"name":"{manifest}","version":"1.0.0"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(
            dir.join("agents").join("lead.md"),
            "---\nname: lead\ndescription: Leads work\nmodel: opus\n---\n# Lead\n",
        )
        .unwrap();
    }

    fn write_agent(path: &Path, frontmatter: &str, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("---\n{frontmatter}\n---\n{body}\n")).unwrap();
    }

    fn owned_agents(home: &Path, plugin: &str) -> Vec<PathBuf> {
        let mut files = std::fs::read_dir(home.join(".codex/agents"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| is_owned_agent(path, plugin))
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    #[test]
    fn invocation_shapes() {
        let inv = remove_invocation("sample@debug", Path::new("/cwd"));
        assert_eq!(inv.program, "codex");
        assert_eq!(inv.args, ["plugin", "remove", "sample@debug"]);
        assert_eq!(inv.display(), "codex plugin remove sample@debug");
        assert_eq!(
            marketplace_add_invocation(Path::new("/s/p"), Path::new("/cwd")).display(),
            "codex plugin marketplace add /s/p"
        );
        assert_eq!(
            add_invocation("frontend", "frontend", Path::new("/cwd")).display(),
            "codex plugin add frontend@frontend"
        );
    }

    #[test]
    fn sync_adds_marketplace_and_plugin_when_missing() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("frontend");
        write_plugin(&src, "frontend", "frontend");
        let original_agent = std::fs::read_to_string(src.join("agents").join("lead.md")).unwrap();
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            &src,
            Path::new("/cwd"),
        );
        let staged = staged_plugin_path(&store, "frontend");
        assert_eq!(
            runner.lines(),
            vec![
                format!("codex plugin marketplace add {}", staged.display()),
                "codex plugin add frontend@frontend".to_string(),
            ]
        );
        let staged_agent = std::fs::read_to_string(staged.join("agents").join("lead.md")).unwrap();
        assert!(staged_agent.contains("model: gpt-5.6"));
        assert_eq!(
            std::fs::read_to_string(src.join("agents").join("lead.md")).unwrap(),
            original_agent
        );
    }

    #[test]
    fn sync_reregisters_on_drift_then_adds() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("flutter");
        write_plugin(&src, "flutter-pivara", "flutter-pivara");
        write_config(
            &home,
            r#"
[marketplaces.flutter-pivara]
source = "/old/source/plugins/flutter"
"#,
        );
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "flutter",
            "flutter-pivara",
            &src,
            Path::new("/cwd"),
        );
        let staged = staged_plugin_path(&store, "flutter");
        assert_eq!(
            runner.lines(),
            vec![
                "codex plugin marketplace remove flutter-pivara".to_string(),
                format!("codex plugin marketplace add {}", staged.display()),
                "codex plugin add flutter-pivara@flutter-pivara".to_string(),
            ]
        );
    }

    #[test]
    fn sync_readds_an_installed_plugin_so_stage_changes_reach_the_codex_cache() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("frontend");
        write_plugin(&src, "frontend", "frontend");
        let staged = staged_plugin_path(&store, "frontend");
        write_config(
            &home,
            &format!(
                r#"
[marketplaces.frontend]
source = "{}"

[plugins."frontend@frontend"]
enabled = true
"#,
                staged.display()
            ),
        );
        let runner = RecordingRunner::new();
        sync_installed_plugin(
            &runner,
            &home,
            &store,
            "frontend",
            "frontend",
            &src,
            Path::new("/cwd"),
        );
        assert_eq!(
            runner.lines(),
            vec!["codex plugin add frontend@frontend".to_string()]
        );
    }

    #[test]
    fn resolve_prefers_codex_marketplace_json() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("plug");
        std::fs::create_dir_all(dir.join(".codex-plugin")).unwrap();
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".codex-plugin").join("marketplace.json"),
            r#"{"name":"codex-mkt"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".claude-plugin").join("marketplace.json"),
            r#"{"name":"claude-mkt"}"#,
        )
        .unwrap();
        assert_eq!(resolve_codex_marketplace_name(&dir, "plug"), "codex-mkt");
    }

    #[test]
    fn codex_sync_registers_each_markdown_agent_as_required_native_toml() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        let src = store.join("frontend");
        write_plugin(&src, "frontend", "frontend");

        sync_installed_plugin(
            &RecordingRunner::new(),
            &home,
            &store,
            "frontend",
            "frontend",
            &src,
            Path::new("/cwd"),
        );

        let files: Vec<_> = std::fs::read_dir(home.join(".codex/agents"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
            .collect();
        assert_eq!(files.len(), 1);
        let agent: toml::Value =
            toml::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
        assert_eq!(agent["name"].as_str(), Some("lead"));
        assert_eq!(agent["description"].as_str(), Some("Leads work"));
        assert_eq!(agent["model"].as_str(), Some("gpt-5.6"));
        assert_eq!(agent["developer_instructions"].as_str(), Some("# Lead"));
    }

    #[test]
    fn codex_sync_maps_claude_models_effort_and_behavior_fields() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let source = tmp.path().join("plugin");
        write_agent(
            &source.join("agents/reviewer.md"),
            "name: reviewer\ndescription: |\n  Reviews plans\n  safely\nmodel: claude-fable-5\neffort: high\nskills:\n  - software-engineer:critique-plan\ntools: Read, Grep\ndisallowedTools: Bash\ncolor: violet",
            "Review the implementation.",
        );

        assert_eq!(
            sync_registered_agents(&home, "software-engineer", &source).unwrap(),
            1
        );
        let text = std::fs::read_to_string(&owned_agents(&home, "software-engineer")[0]).unwrap();
        let agent: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(agent["description"].as_str(), Some("Reviews plans\nsafely"));
        assert_eq!(agent["model"].as_str(), Some("gpt-5.6"));
        assert_eq!(agent["model_reasoning_effort"].as_str(), Some("high"));
        let instructions = agent["developer_instructions"].as_str().unwrap();
        assert!(instructions.contains("Use these skills when applicable"));
        assert!(instructions.contains("Prefer Codex equivalents"));
        assert!(instructions.contains("Do not use Codex equivalents"));
        assert!(agent.get("skills").is_none());
        assert!(agent.get("tools").is_none());
        assert!(agent.get("disallowedTools").is_none());
        assert!(agent.get("color").is_none());

        for (model, expected) in [
            ("opus", "gpt-5.6"),
            ("sonnet", "gpt-5.6-terra"),
            ("haiku", "gpt-5.6-luna"),
        ] {
            let rendered = render_codex_agent(&format!(
                "---\nname: worker\ndescription: Works\nmodel: {model}\n---\nDo work.\n"
            ))
            .unwrap();
            let parsed: toml::Value = toml::from_str(&rendered).unwrap();
            assert_eq!(parsed["model"].as_str(), Some(expected));
        }
    }

    #[test]
    fn codex_refresh_removes_only_stale_agents_owned_by_the_plugin() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let frontend = tmp.path().join("frontend");
        let backend = tmp.path().join("backend");
        write_agent(
            &frontend.join("agents/old.md"),
            "name: old\ndescription: Old agent",
            "Do old work.",
        );
        write_agent(
            &backend.join("agents/lead.md"),
            "name: backend_lead\ndescription: Backend agent",
            "Do backend work.",
        );
        sync_registered_agents(&home, "frontend", &frontend).unwrap();
        sync_registered_agents(&home, "backend", &backend).unwrap();
        let old = owned_agents(&home, "frontend")[0].clone();
        let user = home.join(".codex/agents/user.toml");
        std::fs::write(
            &user,
            "name = \"user\"\ndescription = \"User\"\ndeveloper_instructions = \"Stay\"\n",
        )
        .unwrap();

        std::fs::remove_file(frontend.join("agents/old.md")).unwrap();
        write_agent(
            &frontend.join("agents/new.md"),
            "name: new\ndescription: New agent",
            "Do new work.",
        );
        sync_registered_agents(&home, "frontend", &frontend).unwrap();

        assert!(!old.exists());
        assert_eq!(owned_agents(&home, "frontend").len(), 1);
        assert_eq!(owned_agents(&home, "backend").len(), 1);
        assert!(user.exists());
    }

    #[test]
    fn codex_agent_filenames_isolate_plugins_and_nested_paths() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        write_agent(
            &first.join("agents/review/lead.md"),
            "name: first_lead\ndescription: First",
            "Lead first.",
        );
        write_agent(
            &first.join("agents/review-lead.md"),
            "name: first_flat_lead\ndescription: First flat",
            "Lead first flat.",
        );
        write_agent(
            &second.join("agents/review/lead.md"),
            "name: second_lead\ndescription: Second",
            "Lead second.",
        );

        sync_registered_agents(&home, "first", &first).unwrap();
        sync_registered_agents(&home, "second", &second).unwrap();

        let first_files = owned_agents(&home, "first");
        let second_files = owned_agents(&home, "second");
        assert_eq!(first_files.len(), 2);
        assert_eq!(second_files.len(), 1);
        assert_ne!(first_files[0].file_name(), first_files[1].file_name());
        assert!(first_files.iter().all(|path| !second_files.contains(path)));
    }

    #[test]
    fn removing_a_plugin_deletes_only_its_generated_codex_agents() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        write_agent(
            &first.join("agents/lead.md"),
            "name: first_lead\ndescription: First",
            "Lead first.",
        );
        write_agent(
            &second.join("agents/lead.md"),
            "name: second_lead\ndescription: Second",
            "Lead second.",
        );
        sync_registered_agents(&home, "first", &first).unwrap();
        sync_registered_agents(&home, "second", &second).unwrap();
        let user = home.join(".codex/agents/user.toml");
        std::fs::write(
            &user,
            "name = \"user\"\ndescription = \"User\"\ndeveloper_instructions = \"Stay\"\n",
        )
        .unwrap();

        assert_eq!(remove_registered_agents(&home, "first").unwrap(), 1);
        assert!(owned_agents(&home, "first").is_empty());
        assert_eq!(owned_agents(&home, "second").len(), 1);
        assert!(user.exists());
    }
}
