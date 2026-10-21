# Shortcuts for building, installing and using rea.
#
# Run `make help` for the list. Every target wraps a cargo or rea
# command, so nothing here is required; it only saves remembering the flags.

# Where `install` puts rea and rea-stub (in $(PREFIX)/bin). Override
# with `make install PREFIX=...`. Arbitrary default: ~/.local/bin is on PATH in
# most distributions and needs no root.
PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

# The alias file that `aliases` and `check-aliases` read. The repo's copy is an
# example; point this at your own, e.g. `make aliases ALIASES=~/dotfiles/aliases.txt`.
ALIASES ?= aliases.txt

# musl target for the static build, for whatever machine runs make (x86_64,
# aarch64, ...). Assumption: a Linux host, which is all rea supports.
STATIC_TARGET := $(shell uname -m)-unknown-linux-musl

# Extra cargo flags; empty for a normal build, set to --target by the *-static
# targets below and by PIE=no. Not meant to be set by hand.
TARGET_FLAGS :=

# Whether to build position-independent executables: yes (the default) or no.
# A PIE loads at a random address (ASLR) but has to relocate itself at every
# start. For the stub, which starts once per alias call, that measured as about
# 7k user and 25k kernel instructions per call on x86_64 musl. With PIE=no, the
# stack, heap and mmaps are still randomized; only the binary's own code and
# data sit at a fixed address.
PIE ?= yes
ifeq ($(PIE),no)
# Appended to any RUSTFLAGS from the environment rather than replacing them.
# Only for build and install (and the static variants, which run these), so
# test and lint don't rebuild everything when PIE changes. Assumption: rustc
# makes a static non-PIE binary from relocation-model=static with crt-static
# (the musl default), and a dynamic non-PIE one with glibc.
build install: export RUSTFLAGS := $(RUSTFLAGS) -C relocation-model=static
# Without --target, cargo applies RUSTFLAGS to proc macros (clap_derive) too,
# which must stay position-independent and then fail to link. Naming the host
# target keeps RUSTFLAGS to the binaries; the static variants set their own.
# The plain build's output moves to target/HOST-TRIPLE/release as a result.
TARGET_FLAGS = --target $(shell rustc -vV | sed -n 's/^host: //p')
else ifneq ($(PIE),yes)
$(error PIE must be yes or no, not "$(PIE)")
endif

# The installed tool, which `aliases` uses so that aliases are made from the
# installed stub (the one next to it), not from whatever is in target/.
REA := $(BINDIR)/rea

# None of these name files, so make must always run them.
.PHONY: help build build-static test test-matrix lint install install-static uninstall aliases aliases-prune check-aliases clean

# Default target: show what's available, since there is no single obvious
# action (unlike a typical `make` that just builds).
help:
	@echo "make build           release build into target/"
	@echo "make build-static    static (musl) release build"
	@echo "make test            run all tests"
	@echo "make test-matrix     the same in every build configuration (needs the musl target)"
	@echo "make lint            run clippy on everything"
	@echo "make install         install rea + rea-stub into $(BINDIR)"
	@echo "make install-static  the same, statically linked (runs on any Linux of this arch)"
	@echo "make uninstall       remove them again (aliases are left in place)"
	@echo "make aliases         create/update the aliases in $(ALIASES), into $(BINDIR)"
	@echo "make aliases-prune   the same, and remove aliases no longer in $(ALIASES)"
	@echo "make check-aliases   validate $(ALIASES) without writing anything"
	@echo "make clean           remove target/"
	@echo
	@echo "Variables: PREFIX=$(PREFIX)  ALIASES=$(ALIASES)  PIE=$(PIE)"
	@echo "  PIE=no builds non-position-independent binaries: faster alias"
	@echo "  startup, but no address randomization for the binary itself"

build:
	cargo build --release --locked $(TARGET_FLAGS)

test:
	cargo test --locked

# The tests in every configuration that changes what ends up in the stub:
# debug and release, with and without LTO, glibc and static musl. Which parts
# of rea-core a stub keeps depends on all three, and once a stub built
# without LTO for musl carried a stray copy of the slot marker that only that
# configuration showed. Slow, since each line is a separate build.
test-matrix: check-static-target
	cargo test --locked
	cargo test --locked --release
	CARGO_PROFILE_RELEASE_LTO=off cargo test --locked --release
	cargo test --locked --target $(STATIC_TARGET)
	cargo test --locked --release --target $(STATIC_TARGET)
	CARGO_PROFILE_RELEASE_LTO=off cargo test --locked --release --target $(STATIC_TARGET)

lint:
	cargo clippy --locked --all-targets

# --locked: use Cargo.lock, which `cargo install` otherwise ignores.
# Both binaries belong to one package, so this installs them side by side,
# which is where rea looks for the stub.
install:
	cargo install --locked --path crates/rea --root $(PREFIX) $(TARGET_FLAGS)

# make's own SHELL is /bin/sh and isn't taken from the environment, hence
# printenv to get the login shell.
COMPLETION_SHELL ?= $(notdir $(shell printenv SHELL))
COMPLETION_FILE_bash := $(PREFIX)/share/bash-completion/completions/rea
COMPLETION_FILE_zsh  := $(PREFIX)/share/zsh/site-functions/_rea
COMPLETION_FILE_fish := $(PREFIX)/share/fish/vendor_completions.d/rea.fish
COMPLETION_FILE ?= $(COMPLETION_FILE_$(COMPLETION_SHELL))

completions:
	$(if $(COMPLETION_FILE),,$(error no completion location known for "$(COMPLETION_SHELL)"; set COMPLETION_FILE))
	mkdir -p $(dir $(COMPLETION_FILE))
	$(REA) completions $(COMPLETION_SHELL) > $(COMPLETION_FILE).tmp
	mv $(COMPLETION_FILE).tmp $(COMPLETION_FILE)

# The static variants reuse the targets above with --target set. Target-
# specific variables carry over to prerequisites, so `install` sees it too.
build-static install-static: TARGET_FLAGS := --target $(STATIC_TARGET)
build-static: check-static-target build
install-static: check-static-target install

# Fail early with the fix, rather than with cargo's longer error about a
# missing standard library for the target.
.PHONY: check-static-target
check-static-target:
	@rustup target list --installed | grep -qx '$(STATIC_TARGET)' || { \
		echo "The $(STATIC_TARGET) target is not installed. Add it with:"; \
		echo "    rustup target add $(STATIC_TARGET)"; \
		exit 1; }

# Removes only the two binaries cargo installed; cargo doesn't know about the
# aliases, and they keep working without the tool anyway.
uninstall:
	cargo uninstall --root $(PREFIX) rea

# Aliases go into $(BINDIR), next to the installed tool (its default).
aliases:
	$(REA) patch $(ALIASES)

aliases-prune:
	$(REA) patch $(ALIASES) --prune

# Uses the installed tool too, so it validates with the same parser the
# installed aliases run.
check-aliases:
	$(REA) check $(ALIASES)

clean:
	cargo clean
