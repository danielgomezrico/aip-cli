.PHONY: help deps build test fmt lint install uninstall release-dry release

CARGO ?= cargo
PREFIX ?= $(HOME)/.local
BIN := aip-cli

.DEFAULT_GOAL := help

help:
	@echo "Usage: make <target>"
	@echo ""
	@echo "  deps          Install build deps (rustup toolchain + cargo-release)"
	@echo "  build         Build the workspace (release)"
	@echo "  test          Run all unit tests"
	@echo "  fmt           Format code"
	@echo "  lint          Clippy with warnings as errors"
	@echo "  install       Install the aip-cli binary into $(PREFIX)/bin"
	@echo "  uninstall     Remove the installed binary"
	@echo "  release-dry   Preview the next version bump (cargo-release)"
	@echo "  release       Cut a release (bump + tag + changelog)  [LEVEL=patch|minor|major]"
	@echo ""
	@echo "After 'make install':"
	@echo "  $(BIN) mode <name>   # pick + apply a mode (saves .aip-cli.toml)"
	@echo "  $(BIN) enable        # re-apply this folder's saved mode (manual; no auto-on-cd)"
	@echo "Tab completion:"
	@echo "  bash:  source <(\$$($(BIN) completion bash))   (or write to ~/.local/share/bash-completion/completions/$(BIN))"
	@echo "  zsh:   mkdir -p ~/.zfunc && $(BIN) completion zsh > ~/.zfunc/_$(BIN)  (then fpath + compinit)"

deps:
	@command -v rustup >/dev/null 2>&1 || { echo "✗ rustup not found — install from https://rustup.rs"; exit 1; }
	rustup show >/dev/null   # materializes the toolchain pinned in rust-toolchain.toml
	@command -v cargo-release >/dev/null 2>&1 || $(CARGO) install cargo-release
	@echo "✓ deps ready"

build:
	$(CARGO) build --release --locked

test:
	$(CARGO) test --workspace

fmt:
	$(CARGO) fmt --all

lint:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

# Install all build deps and the tool itself into the local machine.
install: deps test
	$(CARGO) install --path crates/cli --root $(PREFIX) --locked --force
	@echo "✓ installed $(BIN) → $(PREFIX)/bin/$(BIN)"
	@echo "  usage:    $(BIN) mode <name>   then later  $(BIN) enable"
	@echo "  complete: source <(\$$($(BIN) completion bash))   (bash)  or  $(BIN) completion zsh > ~/.zfunc/_$(BIN) (zsh)"

uninstall:
	$(CARGO) uninstall $(BIN) --root $(PREFIX) || true
	@echo "✓ removed $(BIN)"

release-dry:
	$(CARGO) release $(LEVEL) --workspace

release:
	$(CARGO) release $(LEVEL) --workspace --execute
