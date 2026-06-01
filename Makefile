# Makefile for the `pax` Bluetooth toolkit.
#
# Primary purpose: build and install the `pax` command-line binary (from the
# pax-cli crate). By default it installs with every Linux-capable feature
# (`linux-all`): the BlueZ + btleplug backends and the 802.1X port-auth
# resolvers, so the CLI is fully functional on Linux out of the box. This needs
# the D-Bus development headers at build time (Fedora/Asahi:
# `sudo dnf install dbus-devel`, Debian/Ubuntu: `sudo apt install libdbus-1-dev`)
# and a running `bluetoothd` at run time. Override FEATURES to change backends:
#
#     make install FEATURES=all-backends     # bluez + btleplug, no port-auth
#     make install FEATURES=btleplug         # cross-platform BLE, no system deps
#     make install FEATURES=                 # mock backend only, pure Rust
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

# Cargo features for the CLI. Defaults to `linux-all` — every Linux-capable
# feature (BlueZ + btleplug backends and the port-auth resolvers) — so a plain
# `make install` produces a fully functional Linux CLI. Set to empty
# (FEATURES=) for the pure-Rust, mock-only build with no system deps.
FEATURES ?= linux-all
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
