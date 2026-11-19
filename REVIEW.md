# Review log

Findings about wrong behavior, incorrect implementation, and comments, names
and whitespace in rea, plus a few in the earlier C++ runtime and Python
generator. Each entry is checked before it is fixed. Refuted suspicions stay in
the log because they often belong in a "Constraint on change" comment.

## Scope

- **Commit**: `f8c0dd4` (v0.7.3, "Merge branch 'save'"). Line numbers refer to
  this commit.
- **Full pass**: every file under `crates/` (both crates, source and tests,
  about 7,600 lines including manifests), read start to end for this log.
- **Not reviewed**: `README.md`, `TODO.md`, `Makefile`, `aliases.txt` and
  `bench/`, except where an entry quotes them.
- **C++ and Python**: `alias_runtime.hpp` and `alias_maker.py` as attached to
  the review conversation (not in this repo). Only the parts that came up while
  comparing them with rea were read; they have not had a full pass.
- **Environment for every test and trace**: x86_64, glibc, Rust 1.97.0, debug
  build unless noted. musl has not been tested.

## Decisions pending

These entries are confirmed, but the fix depends on which behavior you want:

- **B1**: remove `ensure_standard_fds`, so pipelines match sh?
- **B4**: should `new` and `patch` follow a symlinked alias, as `save` follows
  a symlinked alias file?
- **B5**: reject `~`, `#` and `[` like the other shell syntax, or document
  them as literal?
- **B6**: document the SIGPIPE deviation, or keep the caller's ignore in the
  stages?
- **B7**: print the usage line only for wrong argument counts, or for every
  error?

## How to use this log

Each entry has:

- **Where**: file and function, with line numbers at the reviewed commit.
- **Claim**: what the code or its comment says, quoted where possible.
- **Evidence**: the command, trace or reading that supports the status.
- **Status**: `suspected`, `confirmed`, `refuted`, or `proposal` (a change
  that was discussed but is not a defect).
- **Fix**: what to change, and whether the C++ or Python has the same issue.

Order of work: settle the behavior entries (B) first, then the implementation
entries (I), then the comment entries (C and D), so no comment is rewritten for
code that is about to change.

## B: behavior

### B1. `ensure_standard_fds` changes pipeline behavior without preventing any misrouting

- **Where**: `crates/core/src/run.rs`, `ensure_standard_fds` (line 314), called
  from `crates/core/src/lib.rs:62`.
- **Claim**: "Without it, a closed fd 2 would be the first one a pipe received,
  and the supervisor's own error messages ("command not found") would be
  written into a stage's input."
- **Evidence**: built the stub with the call commented out and traced it with
  `strace -f`.
  - fd 2 closed, `true | nosuchcmd | cat`: `pipe2` returned `[2, 3]`, so the
    pipe's read end took the gap. The supervisor's `write(2, ...)` calls failed
    with `EBADF` because fd 2 was a read end. The message was lost, and no
    stage received it. With the call, the message goes to `/dev/null`, so it is
    lost either way. Exit status 127 in both cases.
  - fd 0 closed, `cat | cat`: `pipe2` returned `[0, 3]`. The parent ran
    `fcntl(0, F_DUPFD_CLOEXEC, 3) = 3` and the second stage ran `dup2(3, 0)`.
    This is std's `Stdio::Fd` handling in `to_child_stdio`, which duplicates a
    source descriptor out of 0-2 before using it.
  - fds 0 and 1 closed, `echo hi | tee out.txt`: `pipe2` returned `[0, 1]`, so
    the write end sat on its own target. The child ran `fcntl(1, F_GETFD)` and
    `fcntl(1, F_SETFD, 0)` to clear close-on-exec instead of `dup2(1, 1)`.
    The `clone3` flags (`CLONE_VM|CLONE_VFORK`) show this was glibc's
    `posix_spawn`, not std's own code. `echo`'s output reached `tee`.
  - Results, with and without the call, compared to `sh`:

    | Case | sh | without call | with call |
    |---|---|---|---|
    | `echo hi \| tee out.txt`, fds 0 and 1 closed | exit 1, out.txt = `hi`, tee reports "Bad file descriptor" | same as sh | exit 0, out.txt = `hi`, no message |
    | `echo hi \| cat \| cat`, fd 1 closed | exit 1, cat reports "Bad file descriptor" | same as sh | exit 0, no message |
    | `cat \| cat`, fd 0 closed | exit 1, cat reports "Bad file descriptor" | same as sh | exit 0, no message |

  - `cargo test` with the call removed: one failure,
    `alias_runs_with_standard_fds_closed` (`crates/rea/tests/cli.rs:1351`).
    That test asserts the `/dev/null` deviation itself ("A pipeline reopens
    closed descriptors on /dev/null before making its pipes ... so the last
    stage writes there instead"), not correct wiring.
- **Status**: confirmed on glibc. The only observable effect of the call is the
  "Deviation from sh" that its doc comment already lists.
- **Fix** (decision pending): remove `ensure_standard_fds` and its call, so a
  pipeline behaves like `sh` and like the single-command path, which already
  leaves closed descriptors closed. Then:
  - change `alias_runs_with_standard_fds_closed` to expect the pipeline to fail
    the same way `sh` does;
  - update the bullet in the `rea-stub.rs` module doc ("Reopen closed fds 0-2
    on `/dev/null` ... done only when a supervisor runs, by
    [`run::ensure_standard_fds`]") and its link at the bottom of that doc;
  - test on musl first (see Q2).
  If the function is kept, its doc needs a reason that holds (see C9).
- **Counterpart**: B2.

### B2. The C++ runtime's `ensure_std_fds` rests on the same rationale, untested

- **Where**: `alias_runtime.hpp`, `ensure_std_fds` (line 362) and its call in
  `run()` (line 651).
- **Claim**: "a new pipe could be given that number and then be clobbered by
  the dup2s meant for stdin/stdout."
- **Evidence**: none yet. The C++ calls `posix_spawn_file_actions_adddup2`
  directly. In the B1 trace, glibc's `posix_spawn` cleared close-on-exec when
  source and target were the same descriptor, which suggests the C++ is also
  protected in that case. The C++ does nothing like std's duplication of a
  source in 0-2 before `posix_spawn`, so whether the order of its two
  `adddup2` calls can overwrite a source it still needs is the open part.
- **Status**: suspected.
- **Fix**: trace the C++ with the call removed, using the cases from B1.

### B3. A symlink at `write_alias`'s temporary name redirects the write

- **Where**: `crates/rea/src/cli.rs`, `write_alias` (line 615) and
  `write_executable` (line 546).
- **Claim**: "a half-written file must never be left under the alias's name."
  (The rename is safe; the temporary file is not.)
- **Evidence**: the temporary name is fixed (`.{name}.rea-tmp`) and opened with
  `create(true).truncate(true)`, which follows symlinks. Test: with
  `bin/.x.rea-tmp` a symlink to a file `victim`, `rea new x true -o bin`
  printed "created", overwrote `victim` with the stub (now an ELF file,
  7.9 MB), and left `bin/x` as a symlink to `victim`.
- **Status**: confirmed. Exploiting it needs write access to the alias
  directory, so the security impact in a user's own `~/.local/bin` is small;
  the bigger problem is that a stale or planted entry silently damages another
  file.
- **Fix**: create the temporary file the way `replace.rs` does
  (`create_unique`, which uses `create_new` and a random suffix), so nothing
  planted under the name is followed. See also I4.

### B4. `new` and `patch` replace a symlinked alias with a regular file

- **Where**: `crates/rea/src/cli.rs`, `write_alias` (rename at line 616).
- **Claim**: none. `save` follows a symlink on purpose ("A symlink is followed,
  so the file it points to is the one updated"), but `new` and `patch` don't
  say what they do with one.
- **Evidence**: test: `bin/y` a symlink to `dotfiles/y` (an alias holding
  `true`); `rea new y false -o bin` printed "updated", replaced the link with a
  regular file holding `false`, and left `dotfiles/y` holding `true`.
- **Status**: confirmed. Whether this is wrong is a decision: keeping aliases
  in a dotfiles directory and linking them is a plausible setup, and `save`
  already treats links the other way.
- **Fix** (decision pending): follow the link and replace its target, as
  `save` does, or document the current behavior in `new`'s and `patch`'s help.

### B5. `~`, `#` and `[...]` pass through literally where sh would expand them

- **Where**: `crates/core/src/template.rs`, module doc (line 47) and the
  rejected-character arm of `lex` (line 1209).
- **Claim**: "Characters that a shell treats specially but this language does
  not ... are rejected unless escaped or quoted. ... Accepting them silently
  would be worse."
- **Evidence**: `*` and `?` are rejected, but `[` is not, and neither are `~`
  at the start of a word or `#` starting a word.

  | Template | rea | sh |
  |---|---|---|
  | `echo ~` | `~` | `/root` |
  | `echo x # note` | `x # note` | `x` |
  | `echo [ab]1` (files a1, b1 present) | `[ab]1` | `a1 b1` |

  The test `ordinary_runs_stop_at_everything_special` (line 3276) lists
  `~#![]` as ordinary text, so this is current, tested behavior.
- **Status**: confirmed. The behavior contradicts the module's stated
  principle; whether to reject them is a decision. `~` in particular is a
  likely habit (`cp {1} ~/backup`).
- **Fix** (decision pending): reject `~` and `#` at the start of a word and
  `[` anywhere, with the same "quote or escape it" message, and update the
  test and the module doc. Or keep them and document the exception.

### B6. Pipeline stages get SIGPIPE at its default even when the caller ignored it

- **Where**: `crates/core/src/run.rs`, `ignore_sigpipe` (line 271), and the
  `exec_single` doc (line 92).
- **Claim**: `exec_single` keeps an inherited SIGPIPE ignore "which is what
  POSIX shells do with signals ignored on entry". `ignore_sigpipe` says
  "Children are unaffected: `Command` resets SIGPIPE to the default in each
  one."
- **Evidence**: alias `yes | head -1`. Run normally, `yes` was killed by
  SIGPIPE, as expected. Run under `sh -c "trap '' PIPE; ..."`, `yes` was still
  killed by SIGPIPE; under `sh` it would inherit the ignore, get `EPIPE`, and
  exit with an error. So the pipeline path deviates from sh where the
  single-command path matches it.
- **Status**: confirmed. The reset itself is what `ignore_sigpipe` documents;
  what's missing is the deviation, which the matching SIGCHLD case does
  document ("Deviation from sh: the stages get SIGCHLD at its default too").
- **Fix**: add a "Deviation from sh" paragraph to `ignore_sigpipe` (or decide
  to restore the inherited disposition in the stages).

### B7. The template is printed as a usage line for every expansion error

- **Where**: `crates/core/src/lib.rs:50-55`, `run_parsed`.
- **Claim**: "Wrong argument counts get the template as a usage line, since the
  template is the most precise description of what the alias wants."
- **Evidence**: `rea eval 'cat {1:F}' /no/such` and
  `rea eval 'echo {$REA_NOPE}'` both print `    rea = <template>` after the
  error. The code prints it for any `ExpandError`.
- **Status**: confirmed. Code and comment disagree; which is intended is a
  decision. For a failed file check the usage line adds nothing.
- **Fix** (decision pending): print the line only for `WrongArgCount` and
  `EmptyCommand`, or change the comment to say it is printed for every error
  and why.

### B8. `explain` output isn't pasteable for non-UTF-8 arguments

- **Where**: `crates/rea/src/cli.rs`, `quote` (line 1063).
- **Claim**: "Quote a word so the printed command can be pasted into sh (or a
  template)."
- **Evidence**: reading. `quote` starts with `to_string_lossy()`, so a byte
  such as `\xe9` prints as U+FFFD, and pasting the line passes a different
  argument. rea itself passes such arguments through byte for byte
  (`non_utf8_arguments_survive`).
- **Status**: confirmed by reading. Low impact.
- **Fix**: say so in `quote`'s doc, or mark words that were changed by the
  lossy conversion.

## I: implementation

### I1. `relations_into` says it reuses a buffer but allocates one on every call

- **Where**: `crates/core/src/template.rs`, `relations_into` (line 1874).
- **Claim**: "written into `signature` (cleared first), so the arity search can
  reuse one buffer."
- **Evidence**: reading. `signature` is reused, but `ranges` is a new `Vec`
  built on every call.
- **Status**: confirmed by reading. Cost not measured; `ranges` has one entry
  per slice.
- **Fix**: either move `ranges` into `Scratch`, or narrow the doc to say only
  `signature` is reused. Measure before choosing the first.

### I2. The arena returns a pointer made from an integer

- **Where**: `crates/rea/src/bin/rea-stub.rs`, `Arena::alloc` (line 121):
  `Ok(_) => return start as *mut u8,`
- **Claim**: none in the code.
- **Evidence**: reading. The pointer is cast from an address, not derived from
  `self.bytes.get()`, so it has no provenance from the buffer. Miri under
  strict provenance would likely flag it. Miri has not been run.
- **Status**: suspected.
- **Fix**: derive the pointer from the buffer, as in
  `self.bytes.get().cast::<u8>().add(offset)`, which also makes it impossible
  to forget to add the base back (the mistake in the first attempt at the
  padded-buffer change, which returned the bare offset and crashed). See I3.

### I3. Proposal: make the arena buffer 16-aligned and work in offsets

- **Where**: `crates/rea/src/bin/rea-stub.rs`, `Arena` and `Arena::alloc`.
- **Background**: `bytes` follows the 8-byte `used` in a `repr(C, align(16))`
  struct, so the buffer starts at offset 8 and is only 8-aligned. That is why
  `alloc` rounds the address, not the offset. This is load-bearing and
  currently undocumented (see D1).
- **Evidence**: tried during the review with `_pad` before `bytes` (and,
  separately, with a `#[repr(C, align(16))] struct Buffer`), offset rounding,
  an `align > 16` fallback to `System`, and the base added back on return. All
  89 tests passed in dev, release, release without LTO, and non-PIE.
- **Status**: proposal. Not applied. Allocator performance was not
  re-measured.
- **Fix**: if adopted, add
  `const _: () = assert!(std::mem::offset_of!(Arena, bytes) % 16 == 0);`
  and route `align > 16` to `System`. Update the field-order constraint
  comment to mention the padding.

### I4. `write_alias` doesn't sync before renaming, and its temporary name can collide

- **Where**: `crates/rea/src/cli.rs`, `write_alias` (lines 612-619) and
  `write_executable` (line 546).
- **Claim**: "a half-written file must never be left under the alias's name."
- **Evidence**: reading. `write_executable` returns after `write_all` without
  `sync_all`, and the directory isn't synced after the rename. After a crash
  the rename can be on disk before the data, leaving an empty or partial
  executable under the alias's name on file systems that don't order the two.
  Two `rea` processes writing the same alias also share one temporary name.
  `replace.rs` already handles both (unique `create_new` names, `sync_all` on
  the file and the directory).
- **Status**: confirmed by reading; not reproduced.
- **Fix**: reuse `replace.rs`'s helpers for alias files (with mode 0755), which
  also fixes B3.

### I5. `save` doesn't enforce the contract `merge` documents for its input

- **Where**: `crates/core/src/aliasfile.rs:232` (`merge`'s doc) and
  `crates/rea/src/cli.rs:941` (`distinct_aliases`).
- **Claim**: "`aliases` must have unique, valid names and single-line,
  non-blank templates; the caller checks this."
- **Evidence**: reading. `distinct_aliases` checks uniqueness and line breaks,
  not name validity, reserved names (`rea`, `rea-stub`) or blank templates. The
  names come from alias slots, which `rea` validates when it writes them, but
  `slot::encode` itself only refuses newlines, so a stub patched by anything
  else (or damaged) can hold a name such as `a = b`. `merge` would append
  `a = b = <template>`, which reads back as a different alias.
- **Status**: confirmed by reading. Reaching it needs a slot that `rea` did not
  write.
- **Fix**: run `aliasfile::name_problem`, the reserved-name check and a
  non-blank check in `distinct_aliases`, reporting them like its other
  problems.

### I6. `realloc` copies when shrinking a block that isn't the last one

- **Where**: `crates/rea/src/bin/rea-stub.rs`, `Arena::realloc` (line 150).
- **Evidence**: reading. A shrink always fits where the block is, but unless
  the block is the most recent allocation, the code allocates a new block and
  copies. Correct, just unnecessary.
- **Status**: confirmed by reading. Not measured; shrinking is rare in an alias
  run.
- **Fix**: return `ptr` when `new_size <= layout.size()`. Optional.

### I7. Runtime behavior is tested through `rea eval`, not through the stub

- **Where**: `crates/rea/tests/cli.rs:4-5` and most runtime tests.
- **Claim**: "`rea eval TEMPLATE ARGS...` ... runs a template exactly as a
  patched alias would."
- **Evidence**: reading. `rea eval` starts through std's runtime, which reopens
  closed fds 0-2 before `main`, and `rea`'s `main` sets SIGPIPE to its default.
  The stub does neither, and parses with `parse_for_run` instead of `parse`.
  Signal tests (SIGINT, SIGCHLD, the SIGPIPE in `yes | head`) therefore don't
  exercise the stub's startup; B6, for example, would look the same through
  `eval` for a different reason. Four tests run a real alias
  (`new_creates_a_standalone_alias_and_updates_only_on_change`,
  `patch_writes_only_changes_and_prunes_removed_aliases`,
  `alias_runs_with_standard_fds_closed` and
  `alias_handles_arguments_beyond_its_arena`), and none of them tests signal
  handling.
- **Status**: confirmed by reading.
- **Fix**: correct the module doc, and run the signal tests against an alias
  made with `rea new` as well as through `eval`.

### I8. `crates/rea/Cargo.toml` has a release profile that Cargo ignores

- **Where**: `crates/rea/Cargo.toml:33-40`.
- **Evidence**: `cargo build` prints "warning: profiles for the non root
  package will be ignored, specify profiles at the workspace root". The root
  `Cargo.toml` has the same settings, so nothing is lost.
- **Status**: confirmed.
- **Fix**: delete the duplicate.

### I9. `find_alias` treats any execute bit as executable

- **Where**: `crates/rea/src/cli.rs:1010`.
- **Claim**: "Executable regular files only, as the shell's search".
- **Evidence**: reading. `mode & 0o111 != 0` accepts a file that only its
  group or others may execute; the shell checks whether this user can execute
  it (`access(X_OK)` or the exec itself). The lookup can then pick a file the
  shell would skip.
- **Status**: confirmed by reading. Low impact.
- **Fix**: use `libc::access(path, X_OK)`, or note the approximation in the
  comment.

## C: stale or wrong comments

### C1. `restore_default_sigpipe` contradicts `exec_single`

- **Where**: `crates/core/src/run.rs:250-253`.
- **Claim**: "the commands it starts get SIGPIPE back at its default from
  `Command` either way (see `exec_single`)."
- **Evidence**: `exec_single`'s doc says it calls `execvp` itself, bypassing
  `Command`, so that an inherited `SIG_IGN` stays ignored for the target.
- **Status**: confirmed by reading.
- **Fix**: say this holds for pipeline stages, and that a single command keeps
  the disposition it inherited.

### C2. `exit_with` refers to an ignore that std no longer sets

- **Where**: `crates/core/src/run.rs:403`.
- **Claim**: raise would otherwise hit "std's ignore (SIGPIPE)".
- **Evidence**: the stub skips std's runtime; the ignore now comes from
  `ignore_sigpipe` in the same file.
- **Status**: confirmed by reading.
- **Fix**: point at `ignore_sigpipe`.

### C3. `shield_from_terminal_signals` refers to a file outside the repo

- **Where**: `crates/core/src/run.rs:360`, "(The approach of
  alias_runtime.hpp, ...)".
- **Status**: confirmed.
- **Fix**: remove it, or call the file rea's C++ predecessor.

### C4. `aliasfile.rs` still describes symlink-based aliases

- **Where**: `crates/core/src/aliasfile.rs:12` ("the symlink name it will be
  invoked as") and `:116` ("A duplicate would make lookup order-dependent").
- **Evidence**: aliases are now patched copies of the stub; there is no lookup.
- **Status**: confirmed by reading.
- **Fix**: line 12 -> "the alias name, which is also its file name". Line 116
  -> a duplicate would make which definition `patch` writes depend on line
  order.

### C5. `parse_for_run` says the implicit-`{@}` rule is applied "above"

- **Where**: `crates/core/src/template.rs:1567`.
- **Evidence**: the rule is applied about ten lines below, after the gap and
  contradiction checks.
- **Status**: confirmed by reading.
- **Fix**: move the mention to the code that applies the rule.

### C6. `expand_with` comment reads as history

- **Where**: `crates/core/src/template.rs:2185`, "Report only the first
  problem, as before collecting them all".
- **Status**: confirmed (ambiguous wording).
- **Fix**: "A real run stops at the first problem; `preview` collects them
  all."

### C7. `Multi::Slice` doc has an unclear sentence

- **Where**: `crates/core/src/template.rs:118`, "(No end is equivalent to
  `None`: `{..-1}` stops before the last.)"
- **Status**: confirmed (unclear).
- **Fix**: say what it means. Probably that an omitted end is stored as `None`,
  and that no explicit end is normalized the way a `{1..` start is.

### C8. Lines past the wrap width

- **Where**: `crates/core/src/template.rs:48` (module doc, 103 columns) and
  `crates/rea/src/cli.rs:90` (`SYNTAX` help text, past the width of the lines
  around it).
- **Status**: confirmed.
- **Fix**: rewrap. Long error-message string literals elsewhere in
  `template.rs` are not included here.

### C9. `ensure_standard_fds` comments, if the function is kept

- **Where**: `crates/core/src/run.rs:298-336`.
- **Status**: depends on B1. Skip if the function is removed.
- **Fix**: replace the rationale with one that holds; split the body into
  chunks (poll, early return, reopen); state what `ready < 0` means; explain
  why the `fd != pfd.fd` branch closes the descriptor (opened without
  close-on-exec, so children would inherit it); use `fds.len()` instead of
  `3`; make `exec_single` an intra-doc link.

### C10. C++ comments

- **Where**: `alias_runtime.hpp`.
  - Line 1 says "alias_maker.py v3"; the generator is v4.
  - Lines 172-177 document `multi_range` (Rest and Slice, "the clamping
    below") but sit above `slice_range`. `multi_range` has no doc, and
    line 179 repeats line 175.
  - Line 56 (`Part::Kind`) says Rest is "the arguments no {N} or {-N}
    claims", but Rest also steps around slices.
- **Status**: confirmed by reading.

### C11. `SCAN_LIMIT`'s doc doesn't list `save`

- **Where**: `crates/rea/src/cli.rs:25`, "directory scans (`list`,
  `patch --prune`)".
- **Evidence**: `save` also calls `scan` (line 867).
- **Status**: confirmed by reading.
- **Fix**: add `save`, or say "directory scans (see `scan`)".

### C12. `replace`'s doc says its temporary names can't be predicted

- **Where**: `crates/rea/src/replace.rs:20-22`, "under names nobody else can
  predict or have taken".
- **Evidence**: `suffix`'s own doc (line 183) says it is "Not cryptographic",
  built from the clock and process ID. The safety comes from `create_new`,
  which refuses a name that exists, so a predicted name does no harm.
- **Status**: confirmed by reading.
- **Fix**: "under new names that nothing else can have created first".

### C13. `MAX_POSITION`'s timing example isn't the worst case

- **Where**: `crates/core/src/template.rs:801`, "Checking `{1..5000} {5000}`
  takes ~0.25 second on a release build."
- **Evidence**: measured `rea new` on a release build: `{1..5000} {5000}` took
  0.14-0.16 s, consistent with the doc. Each count also compares every pair of
  slices, so many distinct slices take longer: 50 slices
  (`{1..5000} {2..5000} ...`) 0.53 s, 100 slices 0.89 s, 200 slices (2.3 KB,
  still fits an alias) 1.75 s.
- **Status**: confirmed. The search is still bounded; the doc reads as if the
  example were the bound.
- **Fix**: say the cost also grows with the number of distinct slices, with
  the measured example.

### C14. The integration tests' module doc is out of date

- **Where**: `crates/rea/tests/cli.rs:4-6`.
- **Evidence**: "exactly as a patched alias would" (see I7). The assumed
  userland list omits commands the tests use: `seq`, `touch`, `rm`, `wc`,
  `true`, `false` (and `mkfifo` in `template.rs`'s tests).
- **Status**: confirmed by reading.
- **Fix**: update both.

### C15. Test helpers avoid a libc dependency the tests already have

- **Where**: `crates/rea/tests/cli.rs:176-180` (`libc_sigterm`) and line 224
  (`Some(2)` for SIGINT).
- **Claim**: "without a libc dev-dependency just for one constant".
- **Evidence**: `crates/rea/Cargo.toml` lists `libc` as a dev-dependency, and
  `eval_with_sigchld_ignored` (line 236) calls `libc::signal`.
- **Status**: confirmed.
- **Fix**: use `libc::SIGTERM` and `libc::SIGINT`, and delete `libc_sigterm`.

### C16. The root manifest says the release profile changes no behavior

- **Where**: `Cargo.toml:21`, "None of these change behavior."
- **Evidence**: `panic = "abort"` changes what a panic does (abort with
  SIGABRT instead of unwinding and exiting 101). The next comment says so.
- **Status**: confirmed by reading.
- **Fix**: "None of these change behavior, except that a panic aborts."

### C17. Small wording, spelling and formatting fixes

- `crates/rea/src/cli.rs:1062`, "unless it is plainly safe": cut "plainly"
  (style rule), e.g. "unless every character is in the safe set below".
- British spellings among American ones: `replace.rs:190` "neighbours",
  `template.rs:760` "neighbouring". Everything else uses American spelling
  ("behavior").
- Non-ASCII literals in source, which the style rules ask to escape with a
  comment: `template.rs:3138` (an A with diaeresis in a variable name),
  `:3271` and `:3285` (e with acute accent). For example `\u{e9}` with
  `// e with acute accent`.
- `crates/core/src/slot.rs:12`: the `len` row of the layout table is wider
  than the header row, so the table's columns don't line up.
- `crates/rea/src/main.rs:13`, "(see there)": name the function
  (`restore_default_sigpipe`).
- **Status**: confirmed.

## D: missing intent comments and names (do last)

Places where a comment stating what the code computes would read faster than
the code, or where a name hides its units.

### D1. `Arena::alloc` names and comments

- **Where**: `crates/rea/src/bin/rea-stub.rs:108-123`.
- **Status**: draft wording agreed in review, not applied.
- **Fix**: rename `start` -> `alloc_addr_start` (an address) and `end` ->
  `new_used` (the value `used` takes if the allocation is granted). Head the
  rounding and overflow check with one chunk comment ("Find the next address
  in the arena that matches the requested alignment, or fall back to the
  system allocator if there's no room."). Say why it rounds the address and not
  the offset (`bytes` starts 8 bytes in, so the buffer is only 8-aligned),
  unless I3 is adopted. The CAS comment should say the first attempt fails on
  every allocation after the first, because `used` starts as a guess of 0, not
  only when another thread gets there first. `dealloc` and `realloc` reuse
  `start` and `end` as offsets and need the same renaming.

### D2. Dense expressions without an intent comment

| Where | Suggested comment |
|---|---|
| `template.rs:1416` `check_gaps`, `max_by_key(... Reverse(offset))` | the highest number used, and the first place it appears |
| `template.rs` `check_gaps`, `(1..highest).find(...)` | the lowest number below it that is never used |
| `template.rs` `relations_into`, `overlap` and `contains` | whether the two ranges share a position; whether `outer` encloses `inner` |
| `template.rs` `relations_into`, the signature order | entries are compared position by position, so the push order must not depend on the count |
| `template.rs:1785` `layout_into`, `covering` | the reference that uses position `p`: its `{N}`/`{-N}`, or else the slice containing it |
| `template.rs:2253` `preview_with`, env `filter_map` | keep only the variables that were set to something non-empty |
| `template.rs:502` `Arity` `Display`, `expect` | non-empty because the `([], None)` case returned above |
| `slot.rs:159` `locate`, `match (slots.as_slice(), markers.len())` | one real slot wins; otherwise tell "no marker at all" from "markers, but no slot after any of them" |
| `cli.rs:644` `scan`, sort by name then path | two files can hold the same alias, and the output must be stable |
| `alias_runtime.hpp` `slice_range`, the ternaries | 0 is an open end; positive counts from the front from 1; negative counts from the back, so -1 is the last |
| `alias_maker.py:946` | run the same gap check once from the front and once from the back |
| `alias_maker.py:947` | numbers up to the highest that neither a placeholder nor a slice covers |
| `alias_maker.py:670` | the smallest argument count at which the slice is valid |
| `alias_maker.py:782` | each `{N}` and `{-N}` as a one-position interval |
| `cli.rs:959` `distinct_aliases`, `kept.trim() != template.trim()` | templates that differ only in surrounding whitespace are the same once written to the file, which trims them |
| `cli.rs:774, 801, 806, 982` `failures += fail(...)` | relies on `fail` returning 1; say so once, or count with `failures += 1` after `fail` |
| `replace.rs:55` and `:132`, the immediately called closures | what each group of I/O steps is for, and that any error in it lands in one match |

The C++ and Python rows come from the parts of those files that were read; they
have had no full pass.

### D3. Names

- `template.rs:2877`: the test helper `scratch(tag)` (a temporary directory)
  shares its name with the `Scratch` struct (reusable buffers) in the same
  file. Rename the helper, e.g. `temp_dir(tag)`. `tests/cli.rs` has a helper of
  the same name; renaming both keeps them consistent.
- `tests/cli.rs:1351`: `alias_runs_with_standard_fds_closed` also tests
  non-UTF-8 arguments (lines 1377-1393). Split it, or rename it to cover both.
  If B1 removes `ensure_standard_fds`, this test changes anyway.

## Q: open questions

- **Q1.** Before spawning a pipeline's last stage, the supervisor runs
  `fcntl(n, F_GETFD)` on the descriptor that stage inherits (fd 1 in one trace,
  fd 0 in another) and does nothing with the result. Its purpose is unknown.
  The std source is not installed in the review environment.
- **Q2.** musl is untested. B1's same-descriptor case relied on glibc's
  `posix_spawn`; whether std uses `posix_spawn` on musl, and whether musl's
  handles that case, decides whether removing `ensure_standard_fds` is safe
  there. Run `make test-static` with the call removed, plus the B1 cases.
- **Q3.** Run Miri on the arena (I2).
- **Q4.** The review environment runs as root, so
  `save_refuses_a_file_it_may_not_write` skipped itself (`tests/cli.rs:962`)
  and the read-only refusal in `replace` was not exercised. Run the suite as an
  ordinary user before relying on it.

## Checked and holds

- **std wires pipelines correctly when pipe ends land on 0-2** (glibc). Two
  mechanisms cover it: std duplicates a `Stdio::Fd` source out of 0-2 in the
  parent, and glibc's `posix_spawn` clears close-on-exec when a dup2's source
  and target are the same descriptor. Inherited slots holding a close-on-exec
  pipe end close at exec, so the stage gets a closed descriptor, as under `sh`.
- **std uses `posix_spawn` for pipeline stages** (glibc), as the
  `restore_default_sigchld` doc implies. Evidence: `clone3` with
  `CLONE_VM|CLONE_VFORK`.
- **`Arena::alloc` must round the address, not the offset**, with the current
  struct layout. Looks like a simplification candidate but is not (see D1, I3).
- **Pipeline stages get SIGPIPE at its default** although the supervisor
  ignores it (`ignore_sigpipe`'s claim). Evidence: in alias `yes | head -1`,
  `yes` was killed by SIGPIPE. See B6 for the undocumented consequence.
- **`MAX_POSITION`'s timing for its own example** (C13): 0.14-0.16 s measured
  against "~0.25 second".
- **`replace.rs`'s temporary and backup files can't be redirected** by a
  planted name, because `create_unique` uses `create_new` (`O_CREAT|O_EXCL`),
  which refuses existing names, symlinks included. By reading.
- **`merge` numbers lines the way `parse` does**: both treat `\n` and `\r\n`
  as line ends and nothing else, as the comment at `aliasfile.rs:261` says. By
  reading.
