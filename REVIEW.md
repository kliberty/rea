# Review log

Findings from reviewing rea (plus the earlier C++ runtime and Python generator)
for wrong behavior, incorrect implementation, and comments, names and
whitespace. Each entry is checked before it is fixed; refuted suspicions stay
in the log because they often belong in a "Constraint on change" comment.

- Reviewed at: `f8c0dd4` (v0.7.3, "Merge branch 'save'")
- Environment for every test below: x86_64, glibc, Rust 1.97.0, debug build
  unless noted. musl has not been tested.
- C++ and Python files: `alias_runtime.hpp` and `alias_maker.py` as attached to
  the review conversation (not in this repo).

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

This table covers only the code read so far. `template.rs`, `cli.rs`,
`replace.rs` and the tests have not had a full pass.

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
