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

`explain` shows the commands even when a file check would stop them, and
lists those checks after them, so debugging an alias never means changing
files to get past its checks. It also shows the environment variables a
template read, and their values. Its exit status is the alias's own: 2 for a
failed check, an unset variable, a wrong argument count or an invalid template.

`eval -` reads one line from stdin as the template, which saves a layer of
shell quoting (`aliasmux eval - < template.txt`, or a heredoc). Reading it uses
up stdin, so the template's own commands then see stdin at end of file.

## Template syntax

`aliasmux --help` prints this too.

| Syntax | Meaning |
|---|---|
| `{1}`, `{2}`, ... | arguments counted from the front; can be glued to text: `{1}.mkv` |
| `{-1}`, `{-2}`, ... | arguments counted from the back: `{-1}` is the last |
| `{@}` | every argument nothing else uses (no `{N}`, `{-N}` or slice), zero or more |
| `{@+}` | the same, but at least one is required |
| `{A..B}` | a slice: from argument A up to but **not including** B, counted like `{N}` / `{-N}`; `{2..}` is the 2nd onward, `{..-1}` all but the last |
| `{0}` | the alias name |
| `{$NAME}` | environment variable `NAME`, when the alias runs: `rsync -ahP {@+} {$BACKUP_DIR:D}` |
| `{1:/}`, `{1://}` | basename, parent directory (`dir/photo.jpg` gives `photo.jpg`, `dir`) |
| `{1:.}`, `{1:/.}` | without the extension, basename without it (`dir/photo`, `photo`) |
| `{1:F}`, `{1:D}` | must be an existing file (anything but a directory) / directory |
| `{1:E}`, `{1:N}` | must exist / must not exist yet |
| `{:F}`, `{:D}`, `{:E}`, `{:N}` | at the very end of a word, the same checks on the whole word: `{1}.jpg{:N}` |
| `a \| b` | pipe |
| `a && b`, `a \|\| b`, `a ; b` | sequencing, as in sh |
| `'...'` | fully literal |
| `"..."` | literal except references and the escapes `\" \\ \{ \} \$ \`` |
| `\x` | literal `x` |

- A one-argument reference is substituted straight into argv, so a filename
  with spaces is always one word. No quoting is ever needed around `{1}`.
- A word containing `{@}`, `{@+}` or a slice is repeated once per argument:
  `-I{@}` gives `-Ia -Ib`, and nothing at all when there are none (so
  `rm -rf /{@+}` with no arguments never becomes `rm -rf /`). The same
  reference may appear twice in a word (`{@}={@}` gives `a=a b=b`), but two
  different ones may not.
- Slices may overlap `{N}`, `{-N}` and each other on purpose (`{1} {1..}`),
  and they are never empty: `rm {2..}` needs a second argument. `{@}` never
  repeats what a slice already passed: in `{1} {2..-2} {@}` with five
  arguments, `{@}` is the 4th and 5th.
- Every argument must be used, and `{N}` / `{-N}` numbering has no gaps:
  `{3}` needs `{2}`, or a slice that covers it (`{1} {2..} {3}` is fine).
  `{@}` doesn't fill gaps, so the typo `cp {1} {3} {@}` is still caught. The count is exact unless `{@}`, `{@+}` or a slice is
  used, so `flif270 *.flif` is an error, not "process the first file".
  A count at which a slice would overlap the other references differently
  than with more arguments is rejected too: `{1..4} {-1}` takes exactly 4,
  since with 3 the slice would include the last argument. A template that no
  count satisfies is refused when the alias is created.
- Checks and modifiers work on every reference but `{0}`, after a colon:
  `{@:/.}`, `{-1:D}`, `{2..://}`. A check and a modifier combine as
  `{1:F/.}`; the check applies to the argument itself. Modifiers are purely
  textual, as in fd (`a.tar.gz` loses only `.gz`, `.bashrc` has no
  extension, a bare name's parent is `.`). Checks follow symlinks, except `N`,
  for which a dangling symlink counts as existing. `F` is looser than
  `test -f`: it passes anything that isn't a directory, so `/dev/null`,
  named pipes and bash's `<(...)` work as files. They all run before
  anything starts, and a failure exits 2 like a wrong argument count:
  `{1:F} needs an existing file, but 'photos' is a directory`. That also
  means a file an earlier step creates doesn't pass a later check (see
  `TODO.md` for a flag to change that).
- A reference's check is always about the argument, wherever it appears:
  `{1:N}.jpg` checks argument 1, not `….jpg`. To check the path a word
  builds, end the word with a check of its own: `-outfile {1}.jpg{:N}`. It
  checks the whole word, literal text included, so it belongs on words that
  are just a path (`--out={1}{:N}` would check `--out=…`). Checks on the same
  argument that can never both pass, like `{1:F}` and `{1:N}`, are refused
  when the alias is created.
- `{$NAME}` works like a one-argument reference: always exactly one word,
  never split, and it takes checks and modifiers. Unset or empty is an error
  before anything runs, and `explain` shows the values it used. Since the
  value is one word, `CC="zig c++"` names a program called `zig c++`; for a
  command with arguments, make a wrapper (`aliasmux new -o . zigcxx 'zig c++'`)
  and point the variable at that.
- A single command with no references (not even `{0}` or `{$NAME}`) gets
  `{@}` appended, like a shell alias (`ll` above).
- `$` outside `{$NAME}`, backticks, globs, redirection, `( )` and a lone `&` are errors rather
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
