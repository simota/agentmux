# agentmux — build / test / install helpers
# Thin wrapper around cargo for the workspace. Run `make help` for the target list.

CARGO   ?= cargo
PREFIX  ?= /usr/local
DESTDIR ?=
BINDIR   = $(DESTDIR)$(PREFIX)/bin
INSTALL ?= install

# Resolve cargo subcommands (clippy, rustfmt) via the rustup toolchain bin.
# The PATH `cargo` shim may not locate `cargo-clippy`, so prepend the toolchain
# bin (which holds cargo-clippy / cargo-fmt) when rustup is available.
RUSTUP_CARGO := $(shell rustup which cargo 2>/dev/null)
ifneq ($(RUSTUP_CARGO),)
export PATH := $(patsubst %/,%,$(dir $(RUSTUP_CARGO))):$(PATH)
endif

# Shipped binaries (see crates/*/Cargo.toml [[bin]] entries).
BINS       = agentmux agentmux-daemon agentmux-poc
RELEASE_DIR = target/release

# Extra args forwarded to `make run` / `make daemon`, e.g. `make run ARGS="doctor"`.
ARGS ?=

.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "} {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

# --- Build -----------------------------------------------------------------
.PHONY: build
build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

.PHONY: release
release: ## Optimized release build of the whole workspace
	$(CARGO) build --workspace --release

# --- Run -------------------------------------------------------------------
.PHONY: run
run: ## Run the CLI (ARGS="doctor", "project init .", ...)
	$(CARGO) run -p agentmux-cli --bin agentmux -- $(ARGS)

.PHONY: daemon
daemon: ## Run the daemon in the foreground
	$(CARGO) run -p agentmux-daemon -- $(ARGS)

.PHONY: doctor
doctor: ## Run environment diagnostics (release build)
	$(CARGO) run --release -p agentmux-cli --bin agentmux -- doctor

# --- Quality gates (mirror .nexus-loop/verify.sh / CI) ---------------------
.PHONY: test
test: ## Run all unit + integration tests
	$(CARGO) test --workspace

.PHONY: fmt
fmt: ## Format all code
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting without modifying files
	$(CARGO) fmt --all --check

.PHONY: lint
lint: ## Clippy with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: check
check: fmt-check build test lint ## Full local gate: fmt + build + test + clippy

# --- Docs ------------------------------------------------------------------
.PHONY: doc
doc: ## Build API docs for workspace crates
	$(CARGO) doc --workspace --no-deps

# --- Install / uninstall ---------------------------------------------------
.PHONY: install
install: release ## Install release binaries into $(BINDIR) (PREFIX=/usr/local; needs sudo)
	@mkdir -p "$(BINDIR)" 2>/dev/null || true; \
	if [ ! -w "$(BINDIR)" ]; then \
		echo "error: '$(BINDIR)' is not writable. Choose one:"; \
		echo "  sudo make install                   # system-wide (/usr/local)"; \
		echo "  make install PREFIX=\$$HOME/.local   # user-local (no sudo)"; \
		echo "  make install-cargo                  # ~/.cargo/bin (recommended)"; \
		exit 1; \
	fi
	@for bin in $(BINS); do \
		echo "  install $$bin -> $(BINDIR)/$$bin"; \
		$(INSTALL) -m 0755 "$(RELEASE_DIR)/$$bin" "$(BINDIR)/$$bin"; \
	done

.PHONY: uninstall
uninstall: ## Remove installed binaries from $(BINDIR)
	@for bin in $(BINS); do \
		echo "  rm $(BINDIR)/$$bin"; \
		rm -f "$(BINDIR)/$$bin"; \
	done

.PHONY: install-cargo
install-cargo: ## Install binaries into ~/.cargo/bin via cargo install
	$(CARGO) install --path crates/agentmux-cli --force
	$(CARGO) install --path crates/agentmux-daemon --force

# --- Housekeeping ----------------------------------------------------------
.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
