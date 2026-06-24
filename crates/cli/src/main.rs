//! `aip-cli` — install, switch, and auto-activate plugin modes.
//!
//! This binary is intentionally thin: argument parsing and I/O only. All logic
//! lives in `aip_core` so it can be unit-tested without side effects.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

use aip_core::agent_state::read_state;
use aip_core::config::{find_repo_root, MARKER_NAME};
use aip_core::discovery::{discover_plugins, plugins_root};
use aip_core::doctor::{self, AgentInput, ProjectInput, StorePlugin};
use aip_core::hook::{
    self, content_hash, find_marker, hook_script, load_applied_hash, save_applied_hash, AutoAction,
    Marker, MarkerTarget, Shell, TrustStore,
};
use aip_core::ingest::{ingest_folder, ingest_url};
use aip_core::manifest::PluginManifest;
use aip_core::mode_apply::{apply_targets, available, Target, ALL_TARGETS};
use aip_core::modes::{self, resolve};
use aip_core::runner::SystemRunner;
use aip_core::setup::{is_linked, run_setup};
use aip_core::store;

#[derive(Parser)]
#[command(
    name = "aip-cli",
    version,
    about = "Install, switch, and auto-activate Claude Code / Grok plugin modes."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run prepare + install across all discovered plugins.
    Setup {
        /// Repo root (defaults to discovery from the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Enable a curated subset of plugins across every installed AI agent.
    Mode {
        /// Mode selectors (names or 1-based numbers). Omit for an interactive picker.
        selectors: Vec<String>,
        /// Restrict to a single agent (`claude` or `grok`). Default: all installed.
        #[arg(long)]
        only: Option<String>,
    },
    /// Write a `.aip-cli.toml` marker in a folder and trust it so the mode
    /// auto-activates on cd.
    ///
    /// The marker is allowed right after it is written, so no separate
    /// `aip-cli allow` step is needed. By default the marker has no agent
    /// pinned, so the mode applies to every installed AI agent.
    Init {
        /// Mode selectors (names or numbers). Omit for an interactive picker.
        selectors: Vec<String>,
        /// Pin the marker to a single agent (`claude` or `grok`). Default: all.
        #[arg(long)]
        only: Option<String>,
        /// Folder to write the marker into (defaults to the current directory).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Overwrite an existing marker.
        #[arg(long)]
        force: bool,
    },
    /// Copy every plugin found in FOLDER's subdirectories into the .aip-cli store.
    ///
    /// FOLDER is a directory whose immediate subdirectories are each a Claude
    /// plugin (have a `.claude-plugin/plugin.json`). Non-plugin subdirs are
    /// skipped.
    IngestFolder {
        /// Folder whose subdirectories are each a Claude plugin.
        folder: PathBuf,
    },
    /// Clone a plugin from a git URL into the .aip-cli store.
    IngestUrl {
        /// Git URL of a repository containing a single Claude plugin.
        url: String,
    },
    /// List the plugins currently held in the .aip-cli store.
    ListPlugins,
    /// List the available modes and the plugins each enables.
    ListModes,
    /// Print the shell hook to add to your rc file (bash or zsh).
    Hook {
        /// Shell to emit a hook for.
        shell: String,
    },
    /// Trust the marker in DIR (or the current dir) for auto-activation.
    Allow {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Revoke trust for the marker in DIR (or the current dir).
    Deny {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Diagnose the current project, the .aip-cli store, and each AI agent.
    ///
    /// Reports the active marker/mode for the folder, the plugins held in the
    /// store, and which plugins each installed agent (claude, grok) has enabled
    /// — flagging drift, untrusted markers, and plugins missing from the store.
    Doctor {
        /// Folder to diagnose (defaults to the current directory).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Internal: invoked by the shell hook on each `cd`.
    Auto,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Setup { repo } => cmd_setup(repo),
        Command::Mode { selectors, only } => cmd_mode(selectors, only),
        Command::Init {
            selectors,
            only,
            dir,
            force,
        } => cmd_init(selectors, only, dir, force),
        Command::IngestFolder { folder } => cmd_ingest_folder(folder),
        Command::IngestUrl { url } => cmd_ingest_url(url),
        Command::ListPlugins => cmd_list_plugins(),
        Command::ListModes => cmd_list_modes(),
        Command::Hook { shell } => cmd_hook(&shell),
        Command::Allow { dir } => cmd_trust(dir, true),
        Command::Deny { dir } => cmd_trust(dir, false),
        Command::Doctor { dir } => cmd_doctor(dir),
        Command::Auto => cmd_auto(),
    }
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().context("cannot read current directory")
}

fn resolve_repo(repo: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(r) = repo {
        return Ok(r);
    }
    let here = cwd()?;
    find_repo_root(&here).ok_or_else(|| {
        anyhow!("could not find a repo root (no plugins/ or .git above {here:?}); pass --repo")
    })
}

fn cmd_setup(repo: Option<PathBuf>) -> Result<()> {
    // With no explicit --repo, the .aip-cli store is the canonical plugin
    // source. Gate on the *same* predicate setup uses (a discoverable plugin —
    // manifest + Makefile), so a store of manifest-only plugins falls back to
    // repo discovery instead of hard-failing with "no plugins found".
    let root = match repo {
        Some(r) => plugins_root(&r),
        None => {
            let store = store::plugins_dir();
            match discover_plugins(&store) {
                Ok(found) if !found.is_empty() => store,
                _ => plugins_root(&resolve_repo(None)?),
            }
        }
    };
    let plugins =
        discover_plugins(&root).with_context(|| format!("discovering plugins under {root:?}"))?;
    if plugins.is_empty() {
        return Err(anyhow!("no plugins found under {root:?}"));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let runner = SystemRunner;
    println!("→ setup ({} plugins)", plugins.len());
    let steps = run_setup(&plugins, &runner, |p| is_linked(&home, p))?;
    for s in &steps {
        let icon = match s.status {
            "ok" => "✓",
            "skip" => "–",
            _ => "✗",
        };
        println!("  {icon} {:<8} {}", s.phase, s.plugin);
    }
    if steps.iter().any(|s| s.status == "fail") {
        return Err(anyhow!("one or more setup steps failed"));
    }
    Ok(())
}

fn cmd_mode(selectors: Vec<String>, only: Option<String>) -> Result<()> {
    let targets = resolve_targets(only)?;
    let selector = if selectors.is_empty() {
        prompt_for_mode()?
    } else {
        selectors.join(" ")
    };
    apply_selector(&selector, &targets)
}

/// True when `program` is reachable: an absolute path that is a file, or a bare
/// name found on `PATH`.
fn on_path(program: &str) -> bool {
    let p = std::path::Path::new(program);
    if p.is_absolute() {
        return p.is_file();
    }
    match std::env::var_os("PATH") {
        Some(paths) => std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()),
        None => false,
    }
}

/// Every AI agent currently installed.
fn detect_targets() -> Vec<Target> {
    available(&ALL_TARGETS, on_path)
}

/// Decide which agents a manual `mode` invocation targets. `only=Some` restricts
/// to one installed agent; `None` means every installed agent.
fn resolve_targets(only: Option<String>) -> Result<Vec<Target>> {
    match only {
        Some(name) => {
            let t = Target::parse(&name)
                .ok_or_else(|| anyhow!("unknown agent: {name} (use claude or grok)"))?;
            if !on_path(t.program()) {
                return Err(anyhow!(
                    "agent '{}' is not installed ({} not on PATH)",
                    t.key(),
                    t.program()
                ));
            }
            Ok(vec![t])
        }
        None => {
            let found = detect_targets();
            if found.is_empty() {
                let progs: Vec<&str> = ALL_TARGETS.iter().map(|t| t.program()).collect();
                return Err(anyhow!(
                    "no AI agents found on PATH (looked for: {})",
                    progs.join(", ")
                ));
            }
            Ok(found)
        }
    }
}

/// Resolve and apply a selector string against every agent in `targets`.
fn apply_selector(selector: &str, targets: &[Target]) -> Result<()> {
    let resolution = resolve(selector).map_err(|e| anyhow!("{e}"))?;
    let runner = SystemRunner;
    let here = cwd()?;
    let agents: Vec<&str> = targets.iter().map(|t| t.key()).collect();
    println!(
        "→ mode {} → agents: {}",
        resolution.chosen.join(" "),
        agents.join(", ")
    );
    let reports = apply_targets(&resolution, targets, &here, &runner)?;
    for r in reports {
        let enabled: Vec<&str> = r
            .actions
            .iter()
            .filter(|a| a.enable)
            .map(|a| a.plugin)
            .collect();
        let disabled = r.actions.len() - enabled.len();
        println!(
            "  {} ✓ {} ({} disabled)",
            r.target.key(),
            enabled.join(", "),
            disabled
        );
    }
    Ok(())
}

fn cmd_init(selectors: Vec<String>, only: Option<String>, dir: PathBuf, force: bool) -> Result<()> {
    let selector = if selectors.is_empty() {
        prompt_for_mode()?
    } else {
        selectors.join(" ")
    };
    // Validate the selector before writing anything.
    let resolution = resolve(&selector).map_err(|e| anyhow!("{e}"))?;

    // None (the default) => marker has no agent pinned => applies to all agents.
    let target: Option<MarkerTarget> = match only {
        Some(name) => Some(match Target::parse(&name) {
            Some(Target::ClaudeCode) => MarkerTarget::Claude,
            Some(Target::Grok) => MarkerTarget::Grok,
            None => return Err(anyhow!("unknown agent: {name} (use claude or grok)")),
        }),
        None => None,
    };

    let dir = dir
        .canonicalize()
        .with_context(|| format!("no such directory: {dir:?}"))?;
    let marker = dir.join(MARKER_NAME);
    if marker.exists() && !force {
        return Err(anyhow!(
            "{} already exists (use --force to overwrite)",
            marker.display()
        ));
    }

    let body = Marker::render(&selector, target);
    std::fs::write(&marker, &body).with_context(|| format!("writing {}", marker.display()))?;
    println!(
        "✓ wrote {} (mode: {})",
        marker.display(),
        resolution.chosen.join(" ")
    );

    // Trust the marker right after writing it so it auto-activates on cd —
    // allow always runs as the final step of init.
    let mut store = TrustStore::load();
    store.allow(&marker.to_string_lossy(), &content_hash(&body));
    store.save()?;
    println!("✓ trusted — will auto-activate on cd");
    Ok(())
}

fn prompt_for_mode() -> Result<String> {
    println!("Modes (pick one or more):");
    for (i, m) in modes::registry().iter().enumerate() {
        println!("  {:>2}) {:<18} ({})", i + 1, m.key, m.plugins.join(" "));
    }
    print!("MODE? (space-separated numbers or names) ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading mode selection")?;
    Ok(line.trim().to_string())
}

fn cmd_ingest_folder(folder: PathBuf) -> Result<()> {
    let folder = folder
        .canonicalize()
        .with_context(|| format!("no such folder: {folder:?}"))?;
    let dest = store::plugins_dir();
    std::fs::create_dir_all(&dest).with_context(|| format!("creating store {}", dest.display()))?;
    let ingested = ingest_folder(&folder, &dest)
        .with_context(|| format!("ingesting plugins from {}", folder.display()))?;
    if ingested.is_empty() {
        return Err(anyhow!(
            "no plugins found under {} (subdirs need a .claude-plugin/plugin.json)",
            folder.display()
        ));
    }
    println!(
        "→ ingested {} plugin(s) into {}",
        ingested.len(),
        dest.display()
    );
    for i in &ingested {
        println!("  ✓ {}", i.name);
    }
    Ok(())
}

fn cmd_ingest_url(url: String) -> Result<()> {
    let dest = store::plugins_dir();
    std::fs::create_dir_all(&dest).with_context(|| format!("creating store {}", dest.display()))?;
    let runner = SystemRunner;
    let i = ingest_url(&url, &dest, &runner).with_context(|| format!("ingesting {url}"))?;
    println!("→ ingested {} from {}", i.name, url);
    println!("  ✓ {}", i.dest.display());
    Ok(())
}

fn cmd_list_plugins() -> Result<()> {
    let root = store::plugins_dir();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && store::is_plugin_dir(p))
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    if dirs.is_empty() {
        println!("(no plugins in {})", root.display());
        println!("  ingest some with: aip-cli ingest-folder <dir>  or  aip-cli ingest-url <url>");
        return Ok(());
    }
    println!("plugins in {}:", root.display());
    for d in &dirs {
        let name = d.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let version = PluginManifest::read(d)
            .map(|m| m.version)
            .unwrap_or_else(|_| "?".to_string());
        println!("  {name} ({version})");
    }
    Ok(())
}

/// Scan the `.aip-cli` store into a flat list of `(name, version)`, sorted by
/// directory name. Mirrors `cmd_list_plugins`' discovery but returns data.
fn scan_store() -> Vec<StorePlugin> {
    let root = store::plugins_dir();
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && store::is_plugin_dir(p))
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    dirs.iter()
        .map(|d| {
            let name = d
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let version = PluginManifest::read(d)
                .map(|m| m.version)
                .unwrap_or_else(|_| "?".to_string());
            StorePlugin { name, version }
        })
        .collect()
}

fn cmd_doctor(dir: PathBuf) -> Result<()> {
    let here = dir.canonicalize().unwrap_or(dir);
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;

    // Project marker state — cheap local reads.
    let marker = find_marker(&here);
    let marker_text = marker
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let trusted = match (&marker, &marker_text) {
        (Some(p), Some(t)) => TrustStore::load().is_allowed(&p.to_string_lossy(), &content_hash(t)),
        _ => false,
    };
    let applied_hash = load_applied_hash();

    // Gather the independent, potentially slow reads concurrently: the store
    // scan and each agent's on-disk state run on their own threads.
    let (store_plugins, agents) = std::thread::scope(|s| {
        let store_handle = s.spawn(scan_store);
        let agent_handles: Vec<_> = ALL_TARGETS
            .iter()
            .map(|&target| {
                let home = home.as_path();
                s.spawn(move || AgentInput {
                    target,
                    on_path: on_path(target.program()),
                    state: read_state(target, home),
                })
            })
            .collect();
        let store_plugins = store_handle.join().expect("store scan thread panicked");
        let agents = agent_handles
            .into_iter()
            .map(|h| h.join().expect("agent state thread panicked"))
            .collect::<Vec<_>>();
        (store_plugins, agents)
    });

    let project = ProjectInput {
        marker,
        marker_text,
        trusted,
        applied_hash,
    };
    let report = doctor::build_report(project, store_plugins, agents);
    print!("{}", doctor::render(&report));
    Ok(())
}

fn cmd_list_modes() -> Result<()> {
    for (i, m) in modes::registry().iter().enumerate() {
        println!("{:>2}) {:<18} {}", i + 1, m.key, m.plugins.join(" "));
    }
    Ok(())
}

fn cmd_hook(shell: &str) -> Result<()> {
    let shell = Shell::parse(shell)
        .ok_or_else(|| anyhow!("unsupported shell: {shell} (use bash or zsh)"))?;
    let exe = std::env::current_exe()
        .context("cannot resolve own path")?
        .to_string_lossy()
        .to_string();
    print!("{}", hook_script(shell, &exe));
    Ok(())
}

fn cmd_trust(dir: PathBuf, allow: bool) -> Result<()> {
    let dir = dir.canonicalize().with_context(|| format!("{dir:?}"))?;
    let marker = dir.join(MARKER_NAME);
    let marker_str = marker.to_string_lossy().to_string();
    let mut store = TrustStore::load();
    if allow {
        let text = std::fs::read_to_string(&marker)
            .with_context(|| format!("no {MARKER_NAME} in {dir:?}"))?;
        store.allow(&marker_str, &content_hash(&text));
        store.save()?;
        println!("✓ allowed {marker_str}");
    } else if store.deny(&marker_str) {
        store.save()?;
        println!("✓ denied {marker_str}");
    } else {
        println!("– {marker_str} was not trusted");
    }
    Ok(())
}

fn cmd_auto() -> Result<()> {
    let here = cwd()?;
    let marker =
        find_marker(&here).and_then(|p| std::fs::read_to_string(&p).ok().map(|text| (p, text)));
    let trusted = match &marker {
        Some((p, text)) => TrustStore::load().is_allowed(&p.to_string_lossy(), &content_hash(text)),
        None => false,
    };
    let applied = load_applied_hash();
    match hook::decide(marker, trusted, applied.as_deref()) {
        AutoAction::Activate {
            marker,
            mode,
            target,
        } => {
            let text = std::fs::read_to_string(&marker)?;
            // No pinned target => every installed agent; pinned => that one if installed.
            let targets = match target {
                Some(t) if on_path(t.program()) => vec![t],
                Some(t) => {
                    eprintln!(
                        "aip-cli: agent '{}' pinned but not installed; skipping",
                        t.key()
                    );
                    vec![]
                }
                None => detect_targets(),
            };
            if targets.is_empty() {
                eprintln!("aip-cli: no installed agents to apply mode '{mode}'");
            } else {
                apply_selector(&mode, &targets)?;
                // Only record success so it retries once an agent is installed.
                save_applied_hash(&content_hash(&text))?;
            }
        }
        AutoAction::Untrusted { marker } => {
            let dir = marker.parent().unwrap_or(&marker).display();
            eprintln!("aip-cli: {MARKER_NAME} found but not trusted. Run: aip-cli allow {dir}");
        }
        AutoAction::Invalid { marker, error } => {
            eprintln!("aip-cli: invalid {}: {error}", marker.display());
        }
        AutoAction::AlreadyActive | AutoAction::None => {}
    }
    Ok(())
}
