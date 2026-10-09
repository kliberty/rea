# aliasmux

Compiled command aliases with numbered arguments and real pipelines, and no
shell involved. The successor to `alias_maker.py` (2012).

One binary holds every alias. Each alias is a symlink to it, and the name it
was invoked under picks the template, busybox style.

```
# aliases.txt
flif270 = flif -d {1} - | convert -rotate 270 - - | flif -E 14 - {1}.rot.flif && mv {1}.rot.flif {1}
mkvize  = ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv
ll      = ls -la
```

## Packaging variants

The runtime lives in `crates/core`; the other crates only differ in how a
template reaches it:

| Variant | Crate | Each alias is | Needs at run time |
|---|---|---|---|
| multicall | `crates/aliasmux` | a symlink; argv[0] picks the template | the one binary |
| patched stub | `crates/stub` | a copy of a prebuilt stub with the template written into it | nothing |
| shared library | `crates/shared` (cdylib + `c/stub.c`) | a ~20 KB patched C stub calling the runtime `.so` | `lib/libaliasmux_shared.so` beside it |
| single | `crates/single` | its own compiled binary (like alias_maker.py) | nothing |

Build all four side by side into `out/`:

```
scripts/build_variants.py                       # static variants via musl
scripts/build_variants.py --target x86_64-unknown-linux-gnu   # static glibc instead
```

`rustup target add x86_64-unknown-linux-musl` first for the default. The
shared-library variant always uses the gnu target, and needs a C compiler for
its stub.

The shared library is a `cdylib`: its whole interface is one C function,
`aliasmux_run_slot` (declared in `crates/shared/c/aliasmux.h`), with Rust's std
linked inside. So stubs can be written in any language, and the library can be
rebuilt with any rustc without touching existing aliases.

For day-to-day use, pick one:

```
# multicall
cargo build --release
target/release/aliasmux install ~/.local/bin

# patched stubs: no compiler needed per alias, only for the stub itself
target/release/aliasmux patch --stub target/release/aliasmux-stub --out ~/.local/bin
aliasmux inspect ~/.local/bin/flif270      # read the definition back out
```

Templates are parsed at build time (and again by `patch`), so a typo fails
with a caret under the problem. Set `ALIASMUX_ALIASES=path/to/file` to keep the
definitions outside the repo (a dotfiles repo, say).

After editing aliases, rebuild and run `install` again (`--prune` removes links
for deleted aliases) or `patch` again.

## Template syntax

| Syntax | Meaning |
|---|---|
| `{1}`, `{2}`, ... | Nth argument; can be glued to text: `{1}.mkv` |
| `{2..}` | 2nd argument onward, one word each (must be a whole word) |
| `{@}` | all arguments, one word each |
| `a \| b` | pipe |
| `a && b`, `a \|\| b`, `a ; b` | sequencing, as in sh |
| `'...'` | fully literal |
| `"..."` | literal except `{N}` and the escapes `\" \\ \{ \}` |
| `\x` | literal `x` |

- Arguments are substituted straight into argv, so a filename with spaces is
  always one word. No quoting is ever needed around `{1}`.
- A single command with no references gets its arguments appended, like a shell
  alias (`ll` above). Otherwise the argument count must match. `flif270 *.flif`
  is an error, not "process the first file".
- `$`, backticks, globs, redirection, `( )` and a lone `&` are errors unless
  quoted, rather than silently passed through. `find -exec ... '{}' \;` needs
  the braces and `;` quoted, as in sh.

## Runtime behavior

- **Single command**: the alias `exec`s it, so no wrapper process remains, and
  the PID, signals and exit status are the target's own.
- **Pipelines and lists**: a small supervisor spawns each stage directly and
  wires the pipes.
  - A pipeline fails if any stage fails, like `pipefail`. This is what keeps
    `flif270` from `mv`-ing a broken file over the original.
  - Exception: an upstream stage killed by SIGPIPE (as in `yes | head`) counts
    as success.
  - A missing command exits 127, any other spawn failure 126.
  - Ctrl-C reaches every stage. The supervisor waits for them, so stages that
    clean up on SIGINT get to finish. If a stage died by SIGINT, the rest of the
    list is skipped and the supervisor dies by SIGINT too, so calling scripts
    stop as they would under bash.

## Management commands

```
aliasmux list                      # every alias
aliasmux show flif270              # template and argument count
aliasmux explain flif270 'a b.flif'   # print what would run, quoted
aliasmux eval 'printf [%s] {2..}' a b c   # try a template before adding it
aliasmux run flif270 pic.flif      # run without the symlink
aliasmux install [--force] [--prune] DIR
aliasmux patch --stub STUB --out DIR [NAMES...]
aliasmux inspect FILE                # definition stored in a patched alias
```

## Migrating from alias_maker.py

In the old `bash -c` aliases, `$0` was the first argument. So `$0` becomes
`{1}`, `$1` becomes `{2}`, and `"$@"` becomes `{2..}`. Drop the
`bash -c '...'` wrapper and the shell quoting around `$0`.
