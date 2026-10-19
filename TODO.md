# TODO

Ideas that are designed or agreed on but not built yet. Newest first.

## Gather the stub's hot code into fewer pages

Low priority: worth more as a learning exercise than for speed (about 5 µs
per run, against about 100 µs for the exec itself).

Started in [experiments/hot-order](experiments/hot-order/README.md): tools
to trace which functions run and build an ordering file from them, and what
the first measurements showed (a small gain so far).

`perf trace -F all --no-syscalls ~/.local/bin/ALIAS` on the static musl stub
shows 10 page faults of its own, against 4 for a minimal C++ wrapper. Six of
the extra ones are code and read-only data: the code that runs is spread
across about 500 KB, and each fault maps at most one 64 KB window around it
(the kernel's "fault-around"). One trace hit code at `0x7f30`, `0x1d8c0`,
`0x20a00`, `0x63a28` and `0x6ffc8`, and read-only data at `0x84cf0`.

Gathering the hot functions into one or two windows means telling the linker
to place them together, with a symbol-ordering file (`--symbol-ordering-file`
for lld, `--section-ordering-file` for recent GNU ld). Rust already puts each
function in its own section, so ordering is possible. Steps:

1. Build the stub with symbols: `CARGO_PROFILE_RELEASE_STRIP=false`.
2. Run `perf trace -F all --no-syscalls` on an alias made from it.
3. Map each faulting address to a function with `nm -n` on that build.
   Better still, list every function a run executes (for example with
   `perf record` or valgrind's callgrind), since the faults only show the
   first function touched in each window.
4. Write those symbols to an ordering file, link with it, and run the trace
   again to count the windows.

Also seen in the same trace, and harder to fix:

- A fault just past the arena (`anon@0x496420`), probably musl's own globals,
  which the linker places after the stub's 64 KiB arena. With the arena last
  in `.bss` they would share the page already touched at startup.
- A second stack page: some call chain goes deeper than one page.

## Creation-time flags

Per-alias options chosen when the alias is created (`aliasmux new NAME
TEMPLATE --flag`, or in the alias file) and stored in the alias itself, so
they travel with it like the template does.

First use: `--runtime-file-check`. By default, file checks (`{1:F}` etc.)
all run up front, before anything starts. With this flag, each check
runs just before the pipeline that uses it is started instead, so a file an
earlier step creates is visible to a later check: `touch {1} && cat {1:F}`.
"Per pipeline" rather than per stage, because the stages of one pipeline all
start together.

Second use: `--pipefail=MODE`, choosing how a pipeline's exit status is
decided (`pipeline_outcome` in `crates/core/src/run.rs`):

| Mode | Pipeline status | `false \| true` | `yes \| head -1` |
|---|---|---|---|
| `aliasmux` (default, today's rule) | rightmost failing stage, but an upstream stage killed by SIGPIPE counts as success | 1 | 0 |
| `bash` | rightmost failing stage, SIGPIPE included: exactly `set -o pipefail` | 1 | 141 |
| `last` | the last stage alone, as in plain sh | 0 | 0 |

One flag with a value rather than several booleans, because the modes are
mutually exclusive. `bash` is for matching scripts that already rely on
pipefail. `last` gives up the protection that keeps `flif270` from `mv`-ing a
broken file over the original, so the help should say so.

Things to settle:

- **Storage.** The slot payload is `name\ntemplate`. Flags need somewhere to
  live, which means a new payload layout and a slot format version bump
  (`aliasmux:slot:v2`), or a backwards-compatible extension of v1.
- **Alias file syntax**, e.g. `name [--runtime-file-check] = template`.
- **Round trip.** `list` and `inspect` must print the flags too, so that
  `list DIR > aliases.txt` followed by `patch aliases.txt` still recreates
  identical aliases.
- **`eval`** needs the same flags on its command line, so templates can be
  tried out with them.
