# aliasmux

Command aliases with numbered arguments and real pipelines, and no shell
involved. The successor to `alias_maker.py` (2012).

Each alias is a small standalone executable: a copy of `aliasmux-stub` with
the alias written into it. Creating one needs no compiler, and the alias needs
nothing else at run time. Copy it anywhere, under any name, and it still runs.

```
$ aliasmux new flif270 'flif -d {1} - | convert -rotate 270 - - | flif -E 14 - {1}.rot.flif && mv {1}.rot.flif {1}'
created /home/you/.local/bin/flif270
$ flif270 'holiday photo.flif'
```

## Install

```
make install          # aliasmux + aliasmux-stub into ~/.local/bin
make aliases          # create/update the aliases in aliases.txt, there too
```

`make help` lists everything. The variables `PREFIX` (default `~/.local`) and
`ALIASES` (default the example `aliases.txt` here) change where things go and
which alias file is used, e.g. `make aliases ALIASES=~/dotfiles/aliases.txt`.

`make install-static` builds against musl instead, giving fully static
binaries that run on any Linux machine of the same architecture, with no
glibc version to match. It needs the target once:
`rustup target add x86_64-unknown-linux-musl` (`make install-static` prints
the exact command if it's missing). The plain `make install` links glibc
dynamically, so its binaries need a compatible glibc wherever they go.

For a machine with no Rust at all, install there by copying the two binaries
from a static build: they are all aliasmux needs.

`aliasmux` looks for `aliasmux-stub` next to itself, and by default writes
aliases into its own directory too; `--stub` and `-o DIR` override both.

**Upgrading:** `make install aliases`. The new stub differs from the old one,
so every alias is rewritten onto it. `make uninstall` removes the two binaries
and leaves the aliases, which keep working on their own.

Without make, `make install` is
`cargo install --locked --path crates/aliasmux --root ~/.local`
(`--locked` makes cargo use `Cargo.lock`, which `cargo install` otherwise
ignores).

## Creating aliases

One at a time:

```
aliasmux new mkvize 'ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv'
```

Or from an alias file, one `name = template` per line, `#` for comments:

```
# aliases.txt
flif270 = flif -d {1} - | convert -rotate 270 - - | flif -E 14 - {1}.rot.flif && mv {1}.rot.flif {1}
mkvize  = ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv
ll      = ls -la
```

```
aliasmux check aliases.txt            # validate only
aliasmux patch aliases.txt            # create or update them all
aliasmux patch aliases.txt --prune    # ...and remove aliases no longer in the file
aliasmux patch aliases.txt mkvize     # just one
```

- **Validated at creation.** A template is parsed when the alias is created,
  with the same parser the alias runs, so a typo is reported with a caret
  under it and no executable is written. `patch` checks the whole file first
  and reports every problem at once.
- **Only changes are written.** An alias whose definition is unchanged is left
  alone, timestamp included; `patch` lists only what it created, updated or
  removed.
- **Other files are safe.** An existing file is replaced only if it is an
  aliasmux alias. Anything else (another program, the tool, the stub) needs
  `--replace-non-aliases`. `--prune` only ever removes aliases.

The aliases themselves are the record. `aliasmux list DIR` prints the aliases
in a directory as an alias file, so `list ~/.local/bin > aliases.txt` followed
by `patch aliases.txt` recreates them exactly.

```
aliasmux list [DIR]                  # aliases in DIR, as an alias file
aliasmux inspect FILE...             # definition stored in alias executables
aliasmux explain FILE 'a b.flif'     # what an alias would run, quoted
aliasmux explain -t TEMPLATE ARGS    # the same for a template
aliasmux eval TEMPLATE ARGS          # run a template directly, to try it out
aliasmux eval - ARGS                 # the same, with the template read from stdin
```

`eval -` reads one line from stdin as the template, which saves a layer of
shell quoting (`aliasmux eval - < template.txt`, or a heredoc). Reading it uses
up stdin, so the template's own commands then see stdin at end of file.

## Template syntax

| Syntax | Meaning |
|---|---|
| `{1}`, `{2}`, ... | Nth argument; can be glued to text: `{1}.mkv` |
| `{2..}` | 2nd argument onward, one word each (must be a whole word) |
| `{@}` | all arguments, one word each |
| `a \| b` | pipe |
| `a && b`, `a \|\| b`, `a ; b` | sequencing, as in sh |
| `'...'` | fully literal |
| `"..."` | literal except `{N}` and the escapes `\" \\ \{ \} \$ \`` |
| `\x` | literal `x` |

- Arguments are substituted straight into argv, so a filename with spaces is
  always one word. No quoting is ever needed around `{1}`.
- A single command with no references gets its arguments appended, like a shell
  alias (`ll` above). Otherwise the argument count must match. `flif270 *.flif`
  is an error, not "process the first file".
- `$`, backticks, globs, redirection, `( )` and a lone `&` are errors rather
  than silently passed through. Quote or escape them to pass them literally;
  `$` and backticks need `\$` and `` \` `` even inside double quotes, as in sh.
  `find -exec ... '{}' \;` needs the braces and `;` quoted, also as in sh.

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
  - If the alias starts with SIGINT ignored (a background job, or `nohup`), the
    stages inherit that, as they would from a shell.

## Layout

- `crates/core`: the runtime, i.e. template language, alias file format,
  process execution, and the slot format that holds an alias in its stub.
- `crates/aliasmux`: the `aliasmux` tool (`src/main.rs`, `src/cli.rs`) and the
  stub (`src/bin/aliasmux-stub.rs`), built side by side.

Earlier designs (a multicall binary, a shared-library variant with a C stub,
and separately compiled per-alias binaries) are in the git history.

## Migrating from alias_maker.py

In the old `bash -c` aliases, `$0` was the first argument. So `$0` becomes
`{1}`, `$1` becomes `{2}`, and `"$@"` becomes `{2..}`. Drop the
`bash -c '...'` wrapper and the shell quoting around `$0`. A leftover `$0`
is reported rather than passed through, so nothing slips by unconverted.
