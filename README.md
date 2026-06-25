# aip-cli

Enable the right AI plugins, skills, and agents per folder — disable the rest. No uninstalls, no permanent bloat.

Inspired by [direnv](https://direnv.net): drop a marker file in a project, and the moment you `cd` in, the right set turns on and everything else turns off.

## Why

AI agents load every enabled skill and agent into context. The more you have on, the less effective the agent gets — a frontend project doesn't need your Android skills, but they still eat context and crowd out what matters. You also hit hard limits, like the max-skills ceiling, once enough plugins pile up.

The usual fix is to uninstall and reinstall as you switch projects. That's slow and easy to forget.

aip-cli keeps everything installed but only activates what the current folder needs.

## Use cases

- **Match skills to the project.** A frontend repo enables web skills and leaves Android, backend, and infra ones off — the agent stays focused and effective instead of wading through irrelevant tools.
- **Stay under the limits.** Installed enough plugins to blow past the max-skills ceiling? Only the current folder's set counts against it; the rest sit disabled.
- **Use external plugins safely.** Plugins you installed from others run only where they're relevant, not on globally.

## How it works

- Put a `.aip-cli.toml` in a project folder (like `.envrc`).
- On `cd`, it runs `claude plugin enable/disable` (and the same for grok) to turn the right set on and everything else off.
- Change folders → it flips the set. Nothing is ever uninstalled.

## Install

macOS / Linux:

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make install
```

## Quick start

```bash
# once: wire up the shell hook
eval "$(aip-cli hook zsh)"   # or bash

# in a project: write + trust the marker
aip-cli init mobile

# now cd in and out — the right plugins, skills, and agents follow
```

## Marker file

```toml
mode = "mobile"
# target = "grok"   # optional, defaults to all installed agents
```

Trust folders with `aip-cli allow` / `aip-cli deny`, just like direnv — or let `aip-cli init` do it for you.

See `aip-cli --help` for `mode`, `list-modes`, and more.

## Doctor

```bash
aip-cli doctor          # this folder
aip-cli doctor --dir X  # another folder
```

A read-only health check, all in one shot:

- **Project** — the `.aip-cli.toml` in scope, the mode it picks and plugins it enables, whether it's trusted and active.
- **Store** — every plugin in `~/.aip-cli/plugins`, with versions.
- **AI CLIs** — per agent (`claude`, `grok`): installed? which plugins are enabled vs disabled (read from `~/.claude/settings.json` and `~/.grok/config.toml`).

It flags drift (an agent missing a plugin its mode wants), orphans (enabled on an agent but not in the store), and plugins a mode wants that the store lacks. Store and per-agent reads run in parallel.

## Build from source

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make build && make test
```
