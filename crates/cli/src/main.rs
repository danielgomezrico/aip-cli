//! `aip-cli` — install and switch Claude Code / Grok plugin modes.
//!
//! This binary is intentionally thin: argument parsing and I/O only. All logic
//! lives in `aip_core` so it can be unit-tested without side effects.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

use aip_core::agent_state::read_state;
use aip_core::categories;
use aip_core::claude_plugins;
use aip_core::config::{
    canonicalize_dir, canonicalize_or_self, config_dir, find_repo_root, MARKER_NAME,
};
use aip_core::discovery::{discover_plugins, plugins_root};
use aip_core::doctor::{self, AgentInput, ProjectInput, StorePlugin};
use aip_core::grok_plugins;
use aip_core::hook::{
    content_hash, find_marker, load_applied_hash, save_applied_hash, Marker, MarkerTarget,
    TrustStore,
};
use aip_core::ingest::{ingest_folder_with, ingest_url};
use aip_core::manifest::PluginManifest;
use aip_core::mode_apply::{apply_targets, available, is_on_path, Target, ALL_TARGETS};
use aip_core::modes::{self, resolve};
use aip_core::remove::{no_hosts_attempted, remove_from_hosts, RemoveReport, NEITHER_HOST_ERR};
use aip_core::runner::SystemRunner;
use aip_core::setup::{is_linked, run_setup};
use aip_core::store;

#[derive(Parser, Debug)]
#[command(
    name = "aip-cli",
    version,
    about = "Install and switch Claude Code / Grok / Pi plugin modes per folder."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Print each host CLI call (`claude`/`grok` plugin enable|disable, or
    /// `pi install|remove`), its exit code, and its output — so failures are
    /// visible instead of silently tolerated.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run prepare + install across all discovered plugins.
    ///
    /// Optionally ingest plugins from a local folder or git URL first.
    /// If a plugin with the same name already exists in the store, it is replaced.
    Setup {
        /// Local folder or git URL to ingest before setup.
        #[arg(value_name = "SOURCE")]
        source: Option<String>,
        /// Repo root (defaults to discovery from the current directory).
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Pick a mode, apply it now, and remember it for this folder.
    ///
    /// Applies the mode to every installed AI agent, then writes a
    /// `.aip-cli.toml` marker so you can re-apply later with `aip-cli enable`.
    /// Pass `--no-save` for a one-off apply that writes no marker.
    Mode {
        /// Mode selectors (names or 1-based numbers). Omit for an interactive picker.
        selectors: Vec<String>,
        /// Restrict to a single agent (`claude`, `grok`, or `pi`) and pin the
        /// marker to it. Default: all installed agents.
        #[arg(long)]
        only: Option<String>,
        /// Folder to remember the mode in (defaults to the current directory).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Apply once without writing or trusting a marker.
        #[arg(long)]
        no_save: bool,
    },
    /// Apply the mode recorded in this folder's `.aip-cli.toml`.
    ///
    /// Looks up the nearest marker from DIR (default: cwd), then enables that
    /// mode on the installed agents. Does nothing on `cd` — you run this by hand.
    Enable {
        /// Folder to resolve the marker from (defaults to the current directory).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// List the available modes, or the store's plugins with `--plugins`.
    List {
        /// List the plugins held in the .aip-cli store instead of the modes.
        #[arg(long)]
        plugins: bool,
    },
    /// Diagnose the current project, the .aip-cli store, and each AI agent.
    ///
    /// Reports the active marker/mode for the folder, the plugins held in the
    /// store, and which plugins each installed agent (claude, grok, pi) has
    /// enabled — flagging drift, untrusted markers, and plugins missing from the store.
    Doctor {
        /// Folder to diagnose (defaults to the current directory).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Uninstall a named plugin from Claude Code and Codex CLI.
    ///
    /// Writes a gitignored `.aip-removed` in the plugin folder so a later
    /// `setup` will not reinstall it. Does not delete the plugin from the
    /// aip-cli store.
    Remove {
        /// Plugin name, store directory name, or name@marketplace.
        name: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Merge persisted categories for ingested plugins over the built-in catalog,
    // so they participate in modes/doctor just like built-ins.
    modes::set_overlay(categories::load(&config_dir()));
    let verbose = cli.verbose;
    match cli.command {
        Command::Setup { source, repo } => cmd_setup(repo, source, verbose),
        Command::Mode {
            selectors,
            only,
            dir,
            no_save,
        } => cmd_mode(selectors, only, dir, no_save, verbose),
        Command::Enable { dir } => cmd_enable(dir, verbose),
        Command::List { plugins } => {
            if plugins {
                cmd_list_plugins()
            } else {
                cmd_list_modes()
            }
        }
        Command::Doctor { dir } => cmd_doctor(dir),
        Command::Remove { name } => cmd_remove(name, verbose),
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

fn cmd_setup(repo: Option<PathBuf>, source: Option<String>, verbose: bool) -> Result<()> {
    // If a source is provided, ingest it first with automatic overwrite.
    if let Some(source) = source {
        if looks_like_git_url(&source) {
            let runner = SystemRunner { verbose };
            let _ = ingest_url(&source, &store::plugins_dir(), &runner)?;
        } else {
            let folder = PathBuf::from(source);
            let folder = canonicalize_dir(&folder)?;
            let dest = store::plugins_dir();
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("creating store {}", dest.display()))?;
            // Always overwrite when ingesting as part of setup: install_plugin_with
            // does remove_dir_all + copy_dir_all, so an existing slot is replaced
            // whole (no stale internals survive).
            let _ = ingest_folder_with(&folder, &dest, &mut |_| true)?;
        }
    }

    // With no explicit --repo, the .aip-cli store is the canonical plugin
    // source. Gate on the *same* predicate setup uses (a discoverable plugin —
    // manifest + Makefile), so a store of manifest-only plugins falls back to
    // repo discovery instead of hard-failing with "no plugins found".
    // `vendor` is true only when setup runs against a *source repo*, where a
    // plugin's `make prepare` can reach its `../../scripts` vendoring tooling.
    // The store holds already-vendored, self-contained copies, so prepare is
    // skipped there (running it would fail: `../../scripts` doesn't exist).
    let (root, vendor) = match repo {
        Some(r) => (plugins_root(&r), true),
        None => {
            let store = store::plugins_dir();
            match discover_plugins(&store) {
                Ok(found) if !found.is_empty() => (store, false),
                _ => (plugins_root(&resolve_repo(None)?), true),
            }
        }
    };
    let plugins =
        discover_plugins(&root).with_context(|| format!("discovering plugins under {root:?}"))?;
    if plugins.is_empty() {
        return Err(anyhow!("no plugins found under {root:?}"));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let runner = SystemRunner { verbose };
    let store_root = store::plugins_dir();
    println!("→ setup ({} plugins)", plugins.len());
    // Against a source repo we vendor + install normally (`make setup` resolves
    // its `../../scripts` tooling there). Against the store we do a link-only
    // refresh: the copies' `make setup`/`link-cowork` targets chain that same
    // `../../scripts` vendoring, which doesn't exist in the store — so force
    // `make link` (always exit 0) for every plugin. Adding/vendoring a plugin is
    // the job of `setup --repo <source>`, not of a bare store refresh.
    let steps = if vendor {
        run_setup(
            &plugins,
            &runner,
            |p| is_linked(&home, p),
            true,
            &store_root,
        )?
    } else {
        run_setup(&plugins, &runner, |_| true, false, &store_root)?
    };
    for s in &steps {
        let icon = match s.status {
            "ok" => "✓",
            "skip" => "–",
            _ => "✗",
        };
        println!("  {icon} {:<8} {}", s.phase, s.plugin);
    }

    // Against the store, guarantee each plugin is installed for Claude and
    // serving the store's latest code: refresh the marketplace (re-register on
    // installLocation drift, else `marketplace update`), then `plugin install`
    // it if installed_plugins.json doesn't already list it. Keyed by the plugin's
    // manifest + marketplace names (not the store dir). Skipped for a source-repo
    // run (`vendor`): there the marketplace `add` is the plugin's own `make setup`.
    if !vendor && is_on_path("claude") {
        for p in &plugins {
            if aip_core::is_setup_blocked(&p.path, &store_root.join(&p.dir_name)) {
                continue;
            }
            let marketplace = claude_plugins::resolve_marketplace_name(&p.path, &p.dir_name);
            claude_plugins::sync_installed_plugin(
                &runner,
                &home,
                &store_root,
                &p.dir_name,
                &p.name,
                &marketplace,
                &p.path,
            );
        }
    }

    // Ensure grok serves the store's latest code for each plugin. A blind
    // `grok plugin install` hard-fails ("repo already installed") and leaves
    // stale/duplicate registrations, so replace_plugin uninstalls every
    // registration of the name first, then installs fresh from the store path.
    if is_on_path("grok") {
        for p in &plugins {
            if aip_core::is_setup_blocked(&p.path, &store_root.join(&p.dir_name)) {
                continue;
            }
            grok_plugins::replace_plugin(&runner, &p.name, &p.path, &p.path, || {
                grok_plugins::capture_list(&p.path)
            });
        }
    }

    if steps.iter().any(|s| s.status == "fail") {
        return Err(anyhow!("one or more setup steps failed"));
    }
    Ok(())
}

fn cmd_mode(
    selectors: Vec<String>,
    only: Option<String>,
    dir: PathBuf,
    no_save: bool,
    verbose: bool,
) -> Result<()> {
    let selector = if selectors.is_empty() {
        prompt_for_mode()?
    } else {
        selectors.join(" ")
    };
    let targets = resolve_targets(only.clone())?;
    apply_selector(&selector, &targets, verbose)?;
    // Persist the marker by default so `aip-cli enable` can re-apply later.
    if !no_save {
        persist_marker(&selector, only, dir)?;
    }
    Ok(())
}

/// Apply the mode from the nearest `.aip-cli.toml` under `dir` (manual only).
fn cmd_enable(dir: PathBuf, verbose: bool) -> Result<()> {
    let here = canonicalize_dir(&dir)?;
    let marker_path = find_marker(&here).ok_or_else(|| {
        anyhow!("no {MARKER_NAME} found above {here:?}\n  pick a mode first: aip-cli mode <name>")
    })?;
    let text = std::fs::read_to_string(&marker_path)
        .with_context(|| format!("reading {}", marker_path.display()))?;
    let parsed = Marker::parse(&text)
        .with_context(|| format!("invalid marker {}", marker_path.display()))?;
    let targets = match parsed.target.map(Target::from) {
        Some(t) if is_on_path(t.program()) => vec![t],
        Some(t) => {
            return Err(anyhow!(
                "marker pins agent '{}' but {} is not on PATH",
                t.key(),
                t.program()
            ));
        }
        None => detect_targets(),
    };
    if targets.is_empty() {
        return Err(anyhow!(
            "no AI agents found on PATH (looked for: {})",
            ALL_TARGETS
                .iter()
                .map(|t| t.program())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    println!("→ enable from {}", marker_path.display());
    apply_selector(&parsed.mode, &targets, verbose)?;
    save_applied_hash(&content_hash(&text))?;
    Ok(())
}

/// Every AI agent currently installed.
fn detect_targets() -> Vec<Target> {
    available(&ALL_TARGETS, is_on_path)
}

/// Decide which agents a manual `mode` invocation targets. `only=Some` restricts
/// to one installed agent; `None` means every installed agent.
fn resolve_targets(only: Option<String>) -> Result<Vec<Target>> {
    match only {
        Some(name) => {
            let t = Target::parse(&name)
                .ok_or_else(|| anyhow!("unknown agent: {name} (use claude, grok, or pi)"))?;
            if !is_on_path(t.program()) {
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
fn apply_selector(selector: &str, targets: &[Target], verbose: bool) -> Result<()> {
    let resolution = resolve(selector).map_err(|e| anyhow!("{e}"))?;
    let runner = SystemRunner { verbose };
    let here = cwd()?;
    let agents: Vec<&str> = targets.iter().map(|t| t.key()).collect();
    println!(
        "→ mode {} → agents: {}",
        resolution.chosen.join(" "),
        agents.join(", ")
    );
    let reports = apply_targets(&resolution, targets, &here, &runner)?;
    let mut any_failed = false;
    for r in reports {
        // An enable the agent rejected (e.g. a renamed/unknown plugin) is *not*
        // actually on — report only the ones that took, and call out the rest.
        let enabled: Vec<&str> = r
            .actions
            .iter()
            .filter(|a| a.enable && a.success)
            .map(|a| a.plugin.as_str())
            .collect();
        let failed: Vec<&str> = r
            .actions
            .iter()
            .filter(|a| a.enable && !a.success)
            .map(|a| a.plugin.as_str())
            .collect();
        let disabled = r.actions.iter().filter(|a| !a.enable).count();
        println!(
            "  {} ✓ {} ({} disabled)",
            r.target.key(),
            enabled.join(", "),
            disabled
        );
        if !failed.is_empty() {
            any_failed = true;
            println!(
                "  {} ✗ failed to enable: {} — {} doesn't know these plugins",
                r.target.key(),
                failed.join(", "),
                r.target.program()
            );
        }
    }
    if any_failed && !verbose {
        println!(
            "  hint: re-run with --verbose to see the underlying error, then check \
             `{} plugin list` — a renamed plugin must be re-registered with the agent",
            targets.first().map(|t| t.program()).unwrap_or("claude")
        );
    }
    Ok(())
}

/// Write a `.aip-cli.toml` marker for `selector` in `dir` so `aip-cli enable`
/// can re-apply later. Overwrites any existing marker: the user just ran
/// `mode`, so the folder's recorded mode follows that choice.
fn persist_marker(selector: &str, only: Option<String>, dir: PathBuf) -> Result<()> {
    // None (the default) => marker has no agent pinned => applies to all agents.
    let target: Option<MarkerTarget> = match only {
        Some(name) => Some(match Target::parse(&name) {
            Some(Target::ClaudeCode) => MarkerTarget::Claude,
            Some(Target::Grok) => MarkerTarget::Grok,
            Some(Target::Pi) => MarkerTarget::Pi,
            None => return Err(anyhow!("unknown agent: {name} (use claude, grok, or pi)")),
        }),
        None => None,
    };

    let dir = canonicalize_dir(&dir)?;
    let marker = dir.join(MARKER_NAME);
    let body = Marker::render(selector, target);
    std::fs::write(&marker, &body).with_context(|| format!("writing {}", marker.display()))?;

    // Keep trust entry for doctor/compat (auto-on-cd is disabled).
    let mut store = TrustStore::load();
    store.allow(&marker.to_string_lossy(), &content_hash(&body));
    store.save()?;
    println!(
        "✓ saved {} — re-apply later with: aip-cli enable",
        marker.display()
    );
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

/// True for sources that should be cloned as a git repo rather than copied as a
/// local folder.
fn looks_like_git_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("git@")
        || s.starts_with("ssh://")
        || s.starts_with("git://")
        || s.ends_with(".git")
}

fn cmd_list_plugins() -> Result<()> {
    let root = store::plugins_dir();
    let dirs: Vec<PathBuf> = store::read_plugin_dirs(&root);
    if dirs.is_empty() {
        println!("(no plugins in {})", root.display());
        println!("  ingest some with: aip-cli setup <folder-or-git-url>");
        return Ok(());
    }
    println!("plugins in {}:", root.display());
    for d in &dirs {
        let name = d.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let version = PluginManifest::read(d)
            .map(|m| m.version)
            .unwrap_or_else(|_| "?".to_string());
        if aip_core::is_removed(d) {
            println!("  {name} ({version}) [removed]");
        } else {
            println!("  {name} ({version})");
        }
    }
    Ok(())
}

/// Scan the `.aip-cli` store into a flat list of `(name, version)`. Delegates
/// to the unit-tested core scanner, which keys each plugin by its manifest name
/// (not its directory name) so doctor matches agents and modes correctly.
fn scan_store() -> Vec<StorePlugin> {
    doctor::scan_store(&store::plugins_dir())
}

fn cmd_doctor(dir: PathBuf) -> Result<()> {
    let here = canonicalize_or_self(dir);
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
                    on_path: is_on_path(target.program()),
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

fn format_host_line(program: &str, success: bool, spec: &str) -> String {
    format!("  {program} {} {spec}", if success { "✓" } else { "✗" })
}

fn remove_status(report: &RemoveReport) -> Result<()> {
    if no_hosts_attempted(report) {
        Err(anyhow!(NEITHER_HOST_ERR))
    } else {
        Ok(())
    }
}

fn cmd_remove(name: String, verbose: bool) -> Result<()> {
    let runner = SystemRunner { verbose };
    let store_root = store::plugins_dir();
    let cwd = cwd()?;
    let report = remove_from_hosts(&runner, is_on_path, &name, &store_root, &cwd);
    for a in &report.attempts {
        println!("{}", format_host_line(a.program, a.success, &report.spec));
    }
    for path in &report.marked {
        println!("  skip {}", path.display());
    }
    remove_status(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aip_core::remove::HostAttempt;
    use clap::error::ErrorKind;

    #[test]
    fn remove_parses_name() {
        let cli = Cli::try_parse_from(["aip-cli", "remove", "flutter"]).unwrap();
        match cli.command {
            Command::Remove { name } => assert_eq!(name, "flutter"),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn remove_parses_qualified_name() {
        let cli = Cli::try_parse_from(["aip-cli", "remove", "foo@bar"]).unwrap();
        match cli.command {
            Command::Remove { name } => assert_eq!(name, "foo@bar"),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn remove_missing_name_is_clap_error() {
        let err = Cli::try_parse_from(["aip-cli", "remove"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn remove_help_names_hosts_and_keeps_store() {
        let err = Cli::try_parse_from(["aip-cli", "remove", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = err.render().to_string();
        assert!(text.contains("Claude"), "{text}");
        assert!(text.contains("Codex"), "{text}");
        assert!(text.contains("Does not delete"), "{text}");
        assert!(text.contains("store"), "{text}");
        assert!(text.contains(".aip-removed"), "{text}");
        assert!(text.contains("setup"), "{text}");
    }

    #[test]
    fn format_host_line_ok_and_fail() {
        assert_eq!(format_host_line("claude", true, "a@b"), "  claude ✓ a@b");
        assert_eq!(format_host_line("codex", false, "a@b"), "  codex ✗ a@b");
    }

    #[test]
    fn remove_status_empty_is_err() {
        let report = RemoveReport {
            spec: "x".into(),
            attempts: vec![],
            marked: vec![],
        };
        let err = remove_status(&report).unwrap_err();
        assert_eq!(err.to_string(), NEITHER_HOST_ERR);
    }

    #[test]
    fn remove_status_failed_attempt_is_ok() {
        let report = RemoveReport {
            spec: "x".into(),
            attempts: vec![HostAttempt {
                program: "claude",
                success: false,
            }],
            marked: vec![],
        };
        assert!(remove_status(&report).is_ok());
    }
}
