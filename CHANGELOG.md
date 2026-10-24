# Changelog

## v0.7.3 (2026-10-23)

### Added

- `rea save FILE [DIR]` updates an alias file from a directory of aliases, the reverse of `patch`.
    - It replaces the template of each alias the file already defines and keeps everything before the template on that line. Every other line is copied unchanged.
    - Aliases the file lacks are appended in name order, in a block at the end of the file that starts with the comment `# added by rea save`. If the file's last comment is already that marker, new aliases are added to its block.
    - A file that doesn't parse is refused, as are two aliases in DIR that share a name but not a template.
    - An unchanged file is not rewritten. `--dry-run` reports what would change without writing.
    - The file is written as Vim's `backupcopy=auto` does. Normally a new file is renamed over the old one. When that would split hard links or lose the owner, the old file is rewritten in place, with a backup beside it until the write is synced. The file and its directory are both synced.
    - A file that `save` cannot open for writing is refused before any backup is made. A symlink is resolved first, so the link stays a link.
- `rea patch --dry-run` decides each alias exactly as a real run does and prints "would create", "would update", "would replace" and "would remove" lines instead of writing. It doesn't create a missing DIR.

### Changed

- `patch` reports a replaced program that wasn't an alias with "which was not a rea alias".
- `patch`'s summary has one count per verb (created, updated, replaced, unchanged, removed and failed). An update that changes only the stub, with the same name and template as before, is listed as "updated PATH (stub only)" and counted separately within the updates, as in "3 updated (2 stub only)".
- A dry run's summary from `patch` or `save` starts with "dry run: ".

## v0.7.2 (2026-10-23)

### Added

- `$REA_ALIAS_DIR` sets the default alias directory when it is set and not empty. It applies to `new`, `patch`, `list`, and the fallback lookup in `explain` and `inspect`. `-o` or `list`'s DIR still overrides it. Without it, the default is still rea's own directory. The stub is still found next to rea.

## v0.7.1 (2026-10-23)

### Added

- `explain` and `inspect` look up a target without a `/` the way the shell finds a command. They use the first executable file in a `$PATH` directory, then the default alias directory as a fallback. A non-alias that shadows an alias is reported as not an alias, with its path. A target containing `/` is read as given.
- A Makefile helper for creating git tags.

## v0.7.0 (2026-10-23)

### Changed

- Reference numbers in `{N}`, `{-N}` and slice ends are limited to 5000, and larger numbers are refused when the template is parsed. Previously any number was accepted, and creating an alias with a large number could take seconds, since the arity check grows with the square of the largest number. A number too large for `usize` gets the same message. The help text and README state the limit.

### Performance

- The arity search reuses its buffers across argument counts instead of allocating new ones for each count. For `{1..1000} {1000}`, `rea new` allocates 1.6 MB in 3,293 blocks instead of 44 MB in 16,264.

## v0.6.2 (2026-10-23)

### Fixed

- An alias started with SIGCHLD ignored (as bash leaves it after `trap '' CHLD`) treated every waited-for step as failed, so `true && echo ok` printed nothing and exited 1. The supervisor now resets SIGCHLD to its default before starting the first stage. Pipeline stages inherit the default rather than the ignore. A single command is still exec'd with SIGCHLD as inherited.

## v0.6.1 (2026-10-21)

### Fixed

- `rea new` and `rea patch` failed with "found 2 alias slot markers" when the stub contained a stray copy of the marker bytes, as a musl release build without LTO does. A marker, current or legacy, now counts only when a well-formed slot follows it. Error messages for a file that isn't a stub or an alias are worded by the command, as "not a rea stub" or "not a rea alias".

### Added

- rea is licensed under MIT OR Apache-2.0, with `LICENSE-MIT` and `LICENSE-APACHE` included in both published packages.
- Package metadata for publishing to crates.io. The README is rea's crates.io page, and rea-core has a short README saying it is rea's runtime and not a general-purpose library.
- `make test-matrix` runs the tests in debug and release, with and without LTO, for glibc and static musl, and with `PIE=no`. The static musl tests can be run separately from the glibc tests.

### Changed

- The rea package includes the workspace's release profile, so building the published crate uses the same settings.
- The Makefile's help lists the `completions` target.

## v0.6.0 (2026-10-21)

### Changed

- aliasmux is renamed to rea. The packages are `rea` and `rea-core`, the binaries are `rea` and `rea-stub`, and help text, error messages and documentation use the new names. Temporary files are named `.NAME.rea-tmp`. The Makefile variable for the installed tool is `REA`.
- New stubs carry the slot marker `rea:alias:slot:1`. The old marker `aliasmux:slot:v1` is still recognized, so aliases made by aliasmux are listed, inspected, and replaced by `patch` without `--replace-non-aliases`.

### Documentation

- The README has a note on upgrading from aliasmux.

## v0.5.2 (2026-10-21)

### Added

- `aliasmux completions bash|zsh|fish|elvish|powershell` prints a shell completion script covering subcommands, options and file names. Path arguments carry value hints, so zsh and fish offer directories where a directory is expected. The script is generated for the name the tool was run as.
- `make completions` installs the completion script into the shell's completion directory under `$(PREFIX)/share` (the login shell by default), or to `COMPLETION_FILE`.
- `make PIE=no` builds non-position-independent binaries, which skip the stub's relocation on every alias call. The default is still `PIE=yes`.

### Documentation

- The README explains that an alias of aliasmux gets aliasmux's completion script rather than one for its own name, and how to cover the alias in bash.

## v0.5.1 (2026-10-19)

### Performance

- The template lexer copies runs of plain text and single-quoted text at once instead of one character at a time. A template with a 125-character program path went from 14,569 to 7,171 instructions per run, and one with a 13-character path from 6,454 to 5,670.

### Added

- Benchmarking helpers in `bench/` for building stubs from git revisions and comparing `perf stat` counts.

## v0.5.0 (2026-10-19)

### Changed

- A single command is exec'd with file descriptors and signal dispositions exactly as the alias received them, the same as running the command directly. A SIGPIPE inherited as ignored stays ignored, and a single command run with stdout closed fails as it would when run directly.
- Closed file descriptors 0–2 are reopened on `/dev/null` only when the alias runs a pipeline or several commands.
- A stack overflow in the stub is a plain segfault.

### Performance

- The stub skips Rust's standard runtime setup before `main`. Its system calls after the dynamic loader go from 26 to 7 on a glibc build.
- The stub no longer calls `getrandom` on every run.
- The stub allocates from a 64 KiB static arena before falling back to the system allocator. On the single-command path it makes no system calls of its own before exec.
- The stub no longer re-checks every accepted argument count on each run, and checks only the count it was given. aliasmux's own instructions per run drop by 20% for `{1} {@}` and by 43% for `{1} {2} {3} {4..-2} {-2} {-1}`.
- The arena's layout causes fewer page faults per run.

## v0.4.0 (2026-10-19)

### Added

- `{$NAME}` is replaced by the environment variable's value when the alias runs. It is always exactly one argument, never split into words, and accepts checks and modifiers, as in `{$BACKUP_DIR:D}`. It doesn't affect arity, and like `{0}` it turns off the implicit trailing `{@}`. An unset or empty variable is an error, reported before anything runs. `explain` still shows the plan in that case and lists the values it read.

### Changed

- When a template uses slices, `{@}` must be one run at the start or end of the arguments. Templates where a slice would split `{@}`, or where `{@}` would take what is left between two slices' ends, are refused when the alias is created. `{@}` may still fill one gap between a single `{N}` and `{-N}` pair, as in `{1} --opt val {@} -o {-1}`.
- The error for a bare `$` suggests `\$` as well as `'$'`, and mentions `{$NAME}`.
- Messages about contradictory checks say "a path" rather than "an argument".

### Documentation

- The README's `rm -rf` example uses `{@+}`.
- Doc comments follow rustdoc conventions, and `cargo doc` builds without warnings.

## v0.3.0 (2026-10-17)

### Added

- References other than `{0}` can end in `:` followed by a file check, a path modifier, or both, with the check first (`{1:F/.}`). Formats apply to each item of `{@}`, `{@+}` and slices, and don't affect arity.
- Path modifiers edit the text as fd's placeholders do, and pass non-UTF-8 names through.
    - `{1:/}` is the basename.
    - `{1://}` is the parent directory, or `.` for a name with no directory component.
    - `{1:.}` removes the last extension.
    - `{1:/.}` is the basename without the last extension.
- File checks look at the file system before any process starts, and a failed check stops the alias with exit status 2. Symlinks are followed, except for `N`.
    - `F` passes for anything that exists and isn't a directory, including `/dev/null`, FIFOs and bash's `<(...)`.
    - `D` passes for an existing directory.
    - `E` passes for anything that exists.
    - `N` passes when nothing exists at the path. A dangling symlink counts as existing.
- A check letter alone at the end of a word, as in `{1}.jpg{:N}`, checks the whole expanded word, once per copy when the word repeats.
- Checks on the same path that can never both pass, such as `{1:F}` with `{1:N}`, are refused when the alias is created.
- `explain` shows the plan even when file checks fail, followed by every distinct failure, and exits 2. A running alias still stops at the first failure.
- The help shows checks combined with modifiers, with a `tar` example.

### Changed

- `explain` exits 2 for a wrong argument count or an invalid template, as the alias does. Running `explain` on a file that isn't an alias still exits 1.

## v0.2.0 (2026-10-17)

### Removed

- The multicall binary and its build-time embedded alias file. Each alias is now a separate copy of a prebuilt stub with its template written into it.

### Added

- `aliasmux new NAME TEMPLATE` creates one alias from the prebuilt stub, with no compiler involved.
- `aliasmux patch FILE` creates and updates aliases from an alias file. It checks the whole file and reports every problem in file order before writing anything. Only aliases whose bytes change are rewritten. An existing file that isn't an alias is replaced only with `--replace-non-aliases`. `--prune` removes aliases the file no longer defines, and nothing else.
- `aliasmux check` for validating alias definitions.
- `aliasmux list DIR` prints a directory's aliases as an alias file.
- The stub is found next to the tool, and that directory is the default output directory.
- `aliasmux eval - ARGS` reads the template as one line from stdin. Embedded newlines are rejected, and the commands then see stdin at EOF.
- New references.
    - `{-N}` counts arguments from the back, so `{-1}` is the last.
    - `{A..B}`, `{A..}` and `{..B}` are slices, counted like `{N}` and `{-N}`. The end is exclusive, as in Python's `[a:b]`, so `{2..4}` is arguments 2 and 3 and `{..-1}` is every argument but the last. `{..B}` may end one past the last argument. Slices are never empty and may overlap `{N}` and `{-N}`.
    - `{@+}` is like `{@}` but requires at least one argument.
    - `{0}` is the alias name.
- The template syntax is described in `aliasmux --help`, and in `new --help` and `eval --help`.
- A Makefile with `install`, `install-static`, `aliases` and `uninstall` targets, configured by `PREFIX` and `ALIASES`. Installation uses `cargo install`.
- `TODO.md` with planned designs.

### Changed

- `{@}` passes only the arguments that no other reference uses. It used to pass every argument.
- `{N..}` keeps its syntax but is now a slice.
- A word containing a reference to several arguments is repeated once per argument, so `-I{@}` expands to `-Ia -Ib`, and to nothing when there are no arguments.
- Templates are checked for which argument counts they accept, and a template that no count satisfies is refused when it is parsed.
    - Every argument must be used.
    - Gaps in `{N}` and `{-N}` numbering are refused unless a slice covers them. `{@}` doesn't fill such gaps, so `cp {1} {3} {@}` is refused.
    - A count at which a slice would overlap the other references differently than with more arguments is refused.
    - A `{@+}` that a slice always leaves empty, as in `{1..} {@+}`, is refused.
    - Unused arguments are reported the way a template refers to them, as in "nothing uses argument {-1}".
- `$` and backticks inside double quotes are rejected, as they already were unquoted, since sh would expand them there. `\$` and `` \` `` are the escapes. Single quotes stay literal.

### Fixed

- `aliasmux` ends quietly when stdout is closed, as in `aliasmux explain ... | head`, instead of panicking.
- Pipeline stages keep an inherited ignored SIGINT or SIGQUIT, so children of a background job are no longer interrupted by Ctrl-C.

## v0.1.0

- Add a multicall prototype with template parser, supervisor and CLI.