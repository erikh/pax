# Makefile for the `pax` Bluetooth toolkit.
#
# Primary purpose: build and install the `pax` command-line binary (from the
# pax-cli crate). The default build is pure Rust with no system dependencies;
# pass FEATURES to turn on a real hardware backend, e.g.
#
#     make install FEATURES=all-backends     # bluez + btleplug (needs libdbus)
#     make install FEATURES=btleplug         # cross-platform BLE only
#
# Install location is chosen by who you are: as root it goes system-wide
# (/usr/local), otherwise into your home directory (no sudo needed). Override
# PREFIX (and DESTDIR) to force a location, following the usual conventions:
#
#     make install                    # -> ~/.local/bin/pax  (regular user)
#     sudo make install               # -> /usr/local/bin/pax  (root)
#     make install PREFIX=/usr        # -> /usr/bin/pax
#
# Make sure ~/.local/bin is on your PATH for the regular-user install.

CARGO   ?= cargo
# Default PREFIX: system-wide for root (uid 0), user-local for everyone else.
PREFIX  ?= $(shell [ "$$(id -u)" -eq 0 ] && echo /usr/local || echo $(HOME)/.local)
DESTDIR ?=
BINDIR   = $(DESTDIR)$(PREFIX)/bin

# CLI crate and the name of the binary it produces.
CLI_CRATE = pax-cli
BIN       = pax

# Optional cargo features for the CLI (e.g. bluez, btleplug, all-backends).
FEATURES ?=
ifeq ($(strip $(FEATURES)),)
FEATURE_FLAGS =
else
FEATURE_FLAGS = --features $(FEATURES)
endif

BUILT_BIN = target/release/$(BIN)

.PHONY: all build install uninstall test doc clean help

all: build

## build: compile the release CLI binary
build:
	$(CARGO) build --release -p $(CLI_CRATE) $(FEATURE_FLAGS)

## install: install the `pax` binary to $(BINDIR)
install: build
	install -d $(BINDIR)
	install -m 0755 $(BUILT_BIN) $(BINDIR)/$(BIN)
	@echo "installed $(BIN) -> $(BINDIR)/$(BIN)"

## uninstall: remove the installed `pax` binary
uninstall:
	rm -f $(BINDIR)/$(BIN)
	@echo "removed $(BINDIR)/$(BIN)"

## test: run the workspace test suite
test:
	$(CARGO) test --workspace

## doc: build the full API documentation (all features)
doc:
	$(CARGO) doc --workspace --no-deps --all-features

## clean: remove build artifacts
clean:
	$(CARGO) clean

## help: list the available targets
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
