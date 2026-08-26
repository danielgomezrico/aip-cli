//! `aip-cli` — install and switch Claude Code / Grok plugins.
//!
//! This binary is intentionally thin: argument parsing and I/O only. All logic
//! lives in `aip_core` so it can be unit-tested without side effects.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};

use aip_core::agent_state::read_state;
use aip_core::claude_plugins;
use aip_core::config::{canonicalize_dir, canonicalize_or_self, find_repo_root, MARKER_NAME};
use aip_core::discovery::{discover_plugins, plugins_root, Plugin};
use aip_core::doctor::{self, AgentInput, ProjectInput, StorePlugin};
use aip_core::grok_plugins;
use aip_core::hook::{
    content_hash, find_marker, load_applied_hash, save_applied_hash, Marker, MarkerTarget,
    TrustStore,
};
use aip_core::ingest::{ingest_folder_with, ingest_url, is_git_url, refresh_from_origin};
use aip_core::manifest::PluginManifest;
use aip_core::mode_apply::{apply_targets, available, is_on_path, Target, ALL_TARGETS};
use aip_core::modes::{self, resolve};
use aip_core::origins;
use aip_core::remove::{
    expand_remove_names, list_removable, list_removable_for_remove, no_hosts_attempted,
    parse_remove_selectors, remove_from_hosts, RemovablePlugin, RemoveReport, RemoveSelectError,
    NEITHER_HOST_ERR,
};
use aip_core::runner::SystemRunner;
use aip_core::setup::{install_pi_packages, is_linked, run_setup};
use aip_core::store;

#[derive(Parser, Debug)]
#[command(
    name = "aip-cli",
    version,
    about = "Install and switch Claude Code / Codex / Grok / Pi plugins per folder.",
    after_help = "Run with no command to refresh every installed plugin (except .aip-removed) from its last source."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
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
    /// Enable every store plugin (except `.aip-removed`), or only the named ones.
    ///
    /// Applies the set to every installed AI agent, then writes a
    /// `.aip-cli.toml` marker so you can re-apply later with `aip-cli enable`.
    /// Pass `--no-save` for a one-off apply that writes no marker.
    Mode {
        /// Plugin selectors (names or 1-based numbers). Omit to enable all store plugins.
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
    /// List numbered selectable plugins, or the store inventory with `--plugins`.
    List {
        /// List the plugins held in the .aip-cli store (versions and [removed]).
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
    /// Uninstall plugins from Claude Code and Codex CLI.
    ///
    /// Pass one or more names (store directory, manifest, or name@marketplace).
    /// Omit names to pick plugins to mark `.aip-removed` (store and, when
    /// present, source). Writes a gitignored `.aip-removed` in each plugin
    /// folder so a later `setup` will not reinstall them. Does not delete
    /// plugins from the aip-cli store.
    Remove {
        /// Plugin names, store directory names, or name@marketplace.
        /// Omit for an interactive picker.
        names: Vec<String>,
    },
    /// Recopy plugins from the folder/URL they were last ingested from.
    ///
    /// Omit names to refresh every installed plugin (same as a bare `aip-cli`).
    /// Skips `.aip-removed`. Simpler than `setup <folder>`: recopies store slots
    /// and refreshes Claude/Codex/Grok. Does not run `make prepare`.
    Refresh {
        /// Plugin names. Omit to refresh every installed (non-removed) plugin.
        names: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let verbose = cli.verbose;
    match cli.command {
        None => cmd_refresh(Vec::new(), verbose),
        Some(Command::Setup { source, repo }) => cmd_setup(repo, source, verbose),
        Some(Command::Mode {
            selectors,
            only,
            dir,
            no_save,
        }) => cmd_mode(selectors, only, dir, no_save, verbose),
        Some(Command::Enable { dir }) => cmd_enable(dir, verbose),
        Some(Command::List { plugins }) => {
            if plugins {
                cmd_list_plugins()
            } else {
                cmd_list_modes()
            }
        }
        Some(Command::Doctor { dir }) => cmd_doctor(dir),
        Some(Command::Remove { names }) => cmd_remove(names, verbose),
        Some(Command::Refresh { names }) => cmd_refresh(names, verbose),
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
    let had_source = source.is_some();
    if let Some(source) = source {
        if is_git_url(&source) {
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
    // --repo without a SOURCE: remember each plugin's source-repo path so a
    // later bare `aip-cli` can recopy from there. SOURCE ingest already recorded.
    if vendor && !had_source {
        let pairs: Vec<(String, String)> = plugins
            .iter()
            .map(|p| (p.dir_name.clone(), p.path.display().to_string()))
            .collect();
        origins::record_many(
            &store::plugins_dir(),
            Some(&root.display().to_string()),
            pairs,
        )?;
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

    // Install every plugin on Claude and Codex (marketplace add + plugin install).
    // Codex/Pi get a staged copy whose agent `model:` uses that host's latest
    // family alias (`gpt-5.6`, `*opus*`); the store stays Claude-native (`opus`).
    for p in &plugins {
        sync_plugin_hosts(
            &runner,
            &home,
            &store_root,
            &p.dir_name,
            &p.name,
            &p.path,
        );
    }

    // Register store/source plugins with pi (package install → settings packages[]).
    // Pi has no `plugin enable|disable`; install is setup-only. Failures count + print;
    // they do not fail setup (Grok parity).
    if is_on_path("pi") {
        println!("→ pi install ({} plugins)", plugins.len());
        println!(
            "  note: pi has no plugin disable; all installed package skills load globally until removed"
        );
        let report = install_pi_packages(&plugins, &store_root, &runner);
        println!(
            "  pi: {} ok, {} failed, {} skipped",
            report.ok, report.failed, report.skipped
        );
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
        let all = modes::all_plugins();
        if all.is_empty() {
            return Err(anyhow!("no plugins in the store to enable"));
        }
        all.join(" ")
    } else {
        selectors.join(" ")
    };
    let targets = resolve_targets(only.clone())?;
    apply_selector(&selector, &targets, verbose)?;
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

fn display_source(source: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = source.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    source.to_string()
}

fn sync_plugin_hosts(
    runner: &SystemRunner,
    home: &Path,
    store_root: &Path,
    dir_name: &str,
    manifest: &str,
    path: &Path,
) {
    if aip_core::is_setup_blocked(path, &store_root.join(dir_name)) {
        return;
    }
    if is_on_path("claude") {
        let marketplace = claude_plugins::resolve_marketplace_name(path, dir_name);
        claude_plugins::sync_installed_plugin(
            runner,
            home,
            store_root,
            dir_name,
            manifest,
            &marketplace,
            path,
        );
    }
    if is_on_path("codex") {
        aip_core::codex_plugins::sync_installed_plugin(
            runner, home, store_root, dir_name, manifest, path, path,
        );
    }
    if is_on_path("grok") {
        grok_plugins::replace_plugin(runner, manifest, path, path, || {
            grok_plugins::capture_list(path)
        });
    }
}

fn cmd_refresh(names: Vec<String>, verbose: bool) -> Result<()> {
    let store_root = store::plugins_dir();
    let cwd = cwd()?;
    let plugins = list_removable(&store_root);
    if plugins.is_empty() {
        println!("(no plugins in {})", store_root.display());
        println!("  ingest some with: aip-cli setup <folder-or-git-url>");
        return Ok(());
    }
    let names = if names.is_empty() {
        plugins.iter().map(|p| p.dir_name.clone()).collect()
    } else {
        expand_remove_names(&names, &plugins)
    };

    let runner = SystemRunner { verbose };
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let mut failed = false;
    let mut any = false;
    for name in &names {
        if origins::resolve_origin(name, &store_root, &cwd).is_none() {
            println!("  – {name}: no last source (run `aip-cli setup <folder>` once)");
            continue;
        }
        any = true;
        match refresh_from_origin(name, &store_root, &cwd, &runner) {
            Ok(ingested) => {
                println!(
                    "→ refresh {} ← {}",
                    ingested.name,
                    display_source(&ingested.source)
                );
                let manifest = PluginManifest::read(&ingested.dest)
                    .map(|m| m.name)
                    .unwrap_or_else(|_| ingested.name.clone());
                sync_plugin_hosts(
                    &runner,
                    &home,
                    &store_root,
                    &ingested.name,
                    &manifest,
                    &ingested.dest,
                );
                if is_on_path("pi") {
                    let _ = install_pi_packages(
                        &[Plugin {
                            dir_name: ingested.name.clone(),
                            name: manifest,
                            version: String::new(),
                            has_prepare: false,
                            path: ingested.dest,
                        }],
                        &store_root,
                        &runner,
                    );
                }
            }
            Err(e) => {
                println!("  ✗ {name}: {e}");
                failed = true;
            }
        }
    }
    if !any && names.iter().all(|n| plugins.iter().any(|p| &p.dir_name == n)) {
        println!(
            "(no last source recorded — run `aip-cli setup <folder>` once, or run from the plugins repo)"
        );
    }
    if failed {
        Err(anyhow!("one or more plugins failed to refresh"))
    } else {
        Ok(())
    }
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
    for line in modes::numbered_plugin_lines() {
        println!("{line}");
    }
    Ok(())
}

fn format_host_line(program: &str, success: bool, spec: &str) -> String {
    format!("  {program} {} {spec}", if success { "✓" } else { "✗" })
}

fn remove_status(any_hosts: bool) -> Result<()> {
    if any_hosts {
        Ok(())
    } else {
        Err(anyhow!(NEITHER_HOST_ERR))
    }
}

fn prompt_for_plugins(plugins: &[RemovablePlugin]) -> Result<Vec<String>> {
    println!("Plugins (pick one or more):");
    for (i, p) in plugins.iter().enumerate() {
        if p.manifest != p.dir_name {
            println!(
                "  {:>2}) {:<18} ({})  {}",
                i + 1,
                p.dir_name,
                p.version,
                p.manifest
            );
        } else {
            println!("  {:>2}) {:<18} ({})", i + 1, p.dir_name, p.version);
        }
    }
    print!("REMOVE? (space-separated numbers or names) ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading plugin selection")?;
    match parse_remove_selectors(line.trim(), plugins) {
        Ok(names) => Ok(names),
        Err(RemoveSelectError::Empty) => Ok(Vec::new()),
        Err(e) => Err(anyhow!("{e}")),
    }
}

fn print_remove_report(report: &RemoveReport) {
    println!("→ remove {}", report.spec);
    for a in &report.attempts {
        println!("{}", format_host_line(a.program, a.success, &report.spec));
    }
    for path in &report.marked {
        println!("  skip {}", path.display());
    }
}

fn cmd_remove(names: Vec<String>, verbose: bool) -> Result<()> {
    let runner = SystemRunner { verbose };
    let store_root = store::plugins_dir();
    let cwd = cwd()?;
    let plugins = list_removable_for_remove(&store_root, &cwd);
    let names = if names.is_empty() {
        if plugins.is_empty() {
            println!("(no plugins to remove in {})", store_root.display());
            return Ok(());
        }
        let selected = prompt_for_plugins(&plugins)?;
        if selected.is_empty() {
            println!("(nothing selected)");
            return Ok(());
        }
        selected
    } else {
        expand_remove_names(&names, &plugins)
    };

    let mut any_hosts = false;
    for name in &names {
        let report = remove_from_hosts(&runner, is_on_path, name, &store_root, &cwd);
        print_remove_report(&report);
        if !no_hosts_attempted(&report) {
            any_hosts = true;
        }
    }
    remove_status(any_hosts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn remove_parses_name() {
        let cli = Cli::try_parse_from(["aip-cli", "remove", "flutter"]).unwrap();
        match cli.command {
            Some(Command::Remove { names }) => assert_eq!(names, ["flutter"]),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn remove_parses_qualified_name() {
        let cli = Cli::try_parse_from(["aip-cli", "remove", "foo@bar"]).unwrap();
        match cli.command {
            Some(Command::Remove { names }) => assert_eq!(names, ["foo@bar"]),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn remove_parses_multiple_names() {
        let cli = Cli::try_parse_from(["aip-cli", "remove", "flutter", "frontend"]).unwrap();
        match cli.command {
            Some(Command::Remove { names }) => assert_eq!(names, ["flutter", "frontend"]),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn remove_parses_without_names() {
        let cli = Cli::try_parse_from(["aip-cli", "remove"]).unwrap();
        match cli.command {
            Some(Command::Remove { names }) => assert!(names.is_empty()),
            _ => panic!("expected Remove"),
        }
    }

    #[test]
    fn no_command_parses_as_home() {
        let cli = Cli::try_parse_from(["aip-cli"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn refresh_parses_names() {
        let cli = Cli::try_parse_from(["aip-cli", "refresh", "frontend", "flutter"]).unwrap();
        match cli.command {
            Some(Command::Refresh { names }) => assert_eq!(names, ["frontend", "flutter"]),
            _ => panic!("expected Refresh"),
        }
    }

    #[test]
    fn refresh_parses_without_names() {
        let cli = Cli::try_parse_from(["aip-cli", "refresh"]).unwrap();
        match cli.command {
            Some(Command::Refresh { names }) => assert!(names.is_empty()),
            _ => panic!("expected Refresh"),
        }
    }

    #[test]
    fn top_level_help_mentions_refresh() {
        let err = Cli::try_parse_from(["aip-cli", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = err.render().to_string();
        assert!(text.contains("no command"), "{text}");
        assert!(text.contains("refresh"), "{text}");
    }

    #[test]
    fn mode_parses_plugin_selectors() {
        let cli = Cli::try_parse_from(["aip-cli", "mode", "apple", "backend-go"]).unwrap();
        match cli.command {
            Some(Command::Mode { selectors, .. }) => {
                assert_eq!(selectors, ["apple", "backend-go"]);
            }
            _ => panic!("expected Mode"),
        }
    }

    #[test]
    fn mode_without_selectors_means_enable_all() {
        let cli = Cli::try_parse_from(["aip-cli", "mode"]).unwrap();
        match cli.command {
            Some(Command::Mode { selectors, .. }) => assert!(selectors.is_empty()),
            _ => panic!("expected Mode"),
        }
    }

    #[test]
    fn list_help_describes_plugins_not_facet_modes() {
        let err = Cli::try_parse_from(["aip-cli", "list", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = err.render().to_string();
        assert!(text.contains("plugin"), "{text}");
        assert!(!text.contains("available modes"), "{text}");
        assert!(!text.contains("mobile"), "{text}");
    }

    #[test]
    fn mode_help_describes_plugin_selectors() {
        let err = Cli::try_parse_from(["aip-cli", "mode", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = err.render().to_string();
        assert!(text.contains("plugin"), "{text}");
        assert!(text.contains("Omit to enable all"), "{text}");
        assert!(!text.contains("Pick a mode"), "{text}");
        assert!(!text.contains("interactive picker"), "{text}");
        assert!(!text.contains("mobile"), "{text}");
    }

    #[test]
    fn display_source_collapses_home() {
        let home = dirs::home_dir().expect("home");
        let path = home.join("projects").join("claude").join("plugins");
        let shown = display_source(&path.to_string_lossy());
        assert_eq!(shown, "~/projects/claude/plugins");
        assert_eq!(display_source("/tmp/x"), "/tmp/x");
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
        assert!(
            text.contains("Omit") || text.contains("interactive"),
            "{text}"
        );
    }

    #[test]
    fn format_host_line_ok_and_fail() {
        assert_eq!(format_host_line("claude", true, "a@b"), "  claude ✓ a@b");
        assert_eq!(format_host_line("codex", false, "a@b"), "  codex ✗ a@b");
    }

    #[test]
    fn remove_status_empty_is_err() {
        let err = remove_status(false).unwrap_err();
        assert_eq!(err.to_string(), NEITHER_HOST_ERR);
    }

    #[test]
    fn remove_status_failed_attempt_is_ok() {
        assert!(remove_status(true).is_ok());
    }
}
