# Shortcuts for building, installing and using aliasmux.
#
# Run `make help` for the list. Every target wraps a cargo or aliasmux
# command, so nothing here is required; it only saves remembering the flags.

# Where `install` puts aliasmux and aliasmux-stub (in $(PREFIX)/bin). Override
# with `make install PREFIX=...`. Arbitrary default: ~/.local/bin is on PATH in
# most distributions and needs no root.
PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin

# The alias file that `aliases` and `check-aliases` read. The repo's copy is an
# example; point this at your own, e.g. `make aliases ALIASES=~/dotfiles/aliases.txt`.
ALIASES ?= aliases.txt

# musl target for the static build, for whatever machine runs make (x86_64,
# aarch64, ...). Assumption: a Linux host, which is all aliasmux supports.
STATIC_TARGET := $(shell uname -m)-unknown-linux-musl

# Extra cargo flags; empty for a normal build, set to --target by the *-static
# targets below. Not meant to be set by hand.
TARGET_FLAGS :=

# The installed tool, which `aliases` uses so that aliases are made from the
# installed stub (the one next to it), not from whatever is in target/.
ALIASMUX := $(BINDIR)/aliasmux

# None of these name files, so make must always run them.
.PHONY: help build build-static test lint install install-static uninstall aliases aliases-prune check-aliases clean

# Default target: show what's available, since there is no single obvious
# action (unlike a typical `make` that just builds).
help:
	@echo "make build           release build into target/"
	@echo "make build-static    static (musl) release build"
	@echo "make test            run all tests"
	@echo "make lint            run clippy on everything"
	@echo "make install         install aliasmux + aliasmux-stub into $(BINDIR)"
	@echo "make install-static  the same, statically linked (runs on any Linux of this arch)"
	@echo "make uninstall       remove them again (aliases are left in place)"
	@echo "make aliases         create/update the aliases in $(ALIASES), into $(BINDIR)"
	@echo "make aliases-prune   the same, and remove aliases no longer in $(ALIASES)"
	@echo "make check-aliases   validate $(ALIASES) without writing anything"
	@echo "make clean           remove target/"
	@echo
	@echo "Variables: PREFIX=$(PREFIX)  ALIASES=$(ALIASES)"

build:
	cargo build --release --locked $(TARGET_FLAGS)

test:
	cargo test --locked

lint:
	cargo clippy --locked --all-targets

# --locked: use Cargo.lock, which `cargo install` otherwise ignores.
# Both binaries belong to one package, so this installs them side by side,
# which is where aliasmux looks for the stub.
install:
	cargo install --locked --path crates/aliasmux --root $(PREFIX) $(TARGET_FLAGS)

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
	cargo uninstall --root $(PREFIX) aliasmux

# Aliases go into $(BINDIR), next to the installed tool (its default).
aliases:
	$(ALIASMUX) patch $(ALIASES)

aliases-prune:
	$(ALIASMUX) patch $(ALIASES) --prune

# Uses the installed tool too, so it validates with the same parser the
# installed aliases run.
check-aliases:
	$(ALIASMUX) check $(ALIASES)

clean:
	cargo clean
