# aip-cli

Enable the right AI plugins, skills, and agents per folder — disable the rest. No uninstalls, no permanent bloat.

Drop a marker file in a project, then run `aip-cli enable` when you want that folder's set on. Nothing auto-switches on `cd`.

## Why

AI agents load every enabled skill and agent into context. The more you have on, the less effective the agent gets — a frontend project doesn't need your Android skills, but they still eat context and crowd out what matters. You also hit hard limits, like the max-skills ceiling, once enough plugins pile up.

The usual fix is to uninstall and reinstall as you switch projects. That's slow and easy to forget.

aip-cli keeps everything installed but only activates what you explicitly enable for the current folder.

## Use cases

- **Match skills to the project.** A frontend repo enables web skills and leaves Android, backend, and infra ones off — the agent stays focused and effective instead of wading through irrelevant tools.
- **Stay under the limits.** Installed enough plugins to blow past the max-skills ceiling? Only the current folder's set counts against it; the rest sit disabled.
- **Use external plugins safely.** Plugins you installed from others run only where they're relevant, not on globally.

## How it works

- Put a `.aip-cli.toml` in a project folder (or let `aip-cli mode` write one).
- Run `aip-cli enable` to apply that mode: `claude`/`grok plugin enable|disable`, and `pi install|remove` for the right set.
- Nothing runs on `cd`. Modes only change when you run `mode` or `enable`.

## Install

macOS / Linux:

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make install
```

## Quick start

```bash
# in a project: pick a mode — applies now and writes .aip-cli.toml
aip-cli mode mobile

# later (same or another shell): re-apply this folder's saved mode
aip-cli enable
```

`aip-cli mode` applies the set to every installed agent *and* writes a
`.aip-cli.toml` marker so you can re-apply with `aip-cli enable`. Pass `--no-save`
for a one-off apply that leaves no marker.

## Marker file

```toml
mode = "mobile"
# target = "grok"   # optional, defaults to all installed agents
```

See `aip-cli --help` for `mode`, `enable`, `setup`, `list`, and more.

## Doctor

```bash
aip-cli doctor          # this folder
aip-cli doctor --dir X  # another folder
```

A read-only health check, all in one shot:

- **Project** — the `.aip-cli.toml` in scope, the mode it picks and plugins it enables, whether it's trusted and active.
- **Store** — every plugin in `~/.aip-cli/plugins`, with versions.
- **AI CLIs** — per agent (`claude`, `grok`, `pi`): installed? which plugins are enabled vs disabled (read from `~/.claude/settings.json`, `~/.grok/config.toml`, and `~/.pi/agent/settings.json`).

It flags drift (an agent missing a plugin its mode wants), orphans (enabled on an agent but not in the store), and plugins a mode wants that the store lacks. Store and per-agent reads run in parallel.

## Build from source

```bash
git clone https://github.com/dan/aip-cli.git && cd aip-cli && make build && make test
```
