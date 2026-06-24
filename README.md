# aip-cli

Enable/disable plugins, skills, and agents per folder. No uninstalls, no permanent bloat.

## How to install

macOS / Linux:

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make install
```

## How to compile

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make build && make test
```

## Why

AI agents load everything you have enabled into context. Keep everything on → constant low context. Uninstalling to switch → annoying.

This tool was built so you can have many plugins installed but only the relevant ones active for the current work — automatically.

## How

- Put a `.aip-cli.toml` in a project folder (like `.envrc`).
- On `cd`, it runs `claude plugin enable/disable` (and same for grok) to turn the right set on and everything else off.
- Change folders → it flips the set. Nothing is uninstalled.

Inspired by direnv.

## Quick start

```bash
# once
eval "$(aip-cli hook zsh)"   # or bash

# in a project
aip-cli init mobile          # writes + trusts .aip-cli.toml

# now cd in/out and the right plugins/skills/agents are active
```

## Marker

```toml
mode = "mobile"
# target = "grok"   # optional, defaults to all installed agents
```

Use `aip-cli allow` / `aip-cli deny` like direnv. Or just `aip-cli init`.

See `aip-cli --help` for `mode`, `list-modes`, etc.

## Doctor

```bash
aip-cli doctor          # this folder
aip-cli doctor --dir X  # another folder
```

Read-only health check, all in one shot:

- **Project** — the `.aip-cli.toml` in scope, the mode it picks and plugins it enables, whether it's trusted and active.
- **Store** — every plugin in `~/.aip-cli/plugins`, with versions.
- **AI CLIs** — per agent (`claude`, `grok`): installed? which plugins enabled vs disabled (read from `~/.claude/settings.json` and `~/.grok/config.toml`).

Flags drift (agent missing a plugin its mode wants), orphans (enabled on an agent but not in the store), and plugins a mode wants that the store lacks. Store and per-agent reads run in parallel.