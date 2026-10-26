# rea

Safer shell aliases, without the shell

Command aliases as standalone executables, with numbered arguments, pipelines and file checks, validated when you create them.

Shell aliases that take arguments, chain commands with `|`, `&&`, `||` and `;` as in sh, and check their inputs before they run, without a shell in between.

```
$ rea new mvt 'mv -t {-1:D} -- {..-1:E}'
created /home/you/.local/bin/mvt

$ mvt 'beach day.png' notes.txt ~/Archive    # move everything into the last argument
$ mvt photo.png ~/Archvie
mvt: {-1:D} needs an existing directory, but '/home/you/Archvie' does not exist
    mvt = mv -t {-1:D} -- {..-1:E}
```

Every alias rea creates is a small executable in a directory on your
`$PATH`. That means:

- **Arguments are never split or globbed.** `{1}` is always exactly one word,
  so filenames with spaces need no quoting inside the template.
- **Mistakes are caught early.** Templates are validated when you create the
  alias, and a wrong argument count or a missing file stops the alias before
  any command starts.
- **You can see what an alias will do.** `rea explain` prints the exact
  commands, fully expanded and quoted, without running anything.
- **Aliases work everywhere.** `xargs`, `find -exec`, `make`, cron, scripts,
  other aliases and any shell can run them, because they are real programs.
  Shell aliases and functions only work inside the shell that defined them.
- **Pipelines fail safely.** If any stage of `a | b` fails, the pipeline fails,
  so a following `&& rm original` doesn't run.

## Contents

- [Install](#install)
- [Your first aliases](#your-first-aliases)
- [Keeping aliases in a file](#keeping-aliases-in-a-file)
- [Seeing what an alias will do](#seeing-what-an-alias-will-do)
- [Template syntax](#template-syntax)
- [Examples](#examples)
- [Command reference](#command-reference)
- [How aliases run](#how-aliases-run)
- [Template rules in detail](#template-rules-in-detail)
- [Installation details](#installation-details)
- [Migrating from alias_maker.py](#migrating-from-alias_makerpy)

## Install

You need a Rust toolchain to build rea (the aliases it creates don't).

```sh
cargo install rea
```

This installs `rea` and `rea-stub` where cargo puts programs, usually
`~/.cargo/bin`. By default rea creates aliases in the same directory as
itself. To keep them apart from the other programs there, choose a directory
for them in your shell's startup file:

```sh
export REA_ALIAS_DIR=~/.local/bin    # any directory on your $PATH
```

To build from a clone of this repository instead, run `make install`. It
puts `rea`, `rea-stub` and your aliases in `~/.local/bin`, and needs no
`REA_ALIAS_DIR`.

For tab completion in bash:

```sh
rea completions bash > ~/.local/share/bash-completion/completions/rea
```

See [Installation details](#installation-details) for zsh and fish, static
builds, other install locations and upgrading.

## Your first aliases

An alias is a name and a template. `{1}`, `{2}`, ... stand for the arguments
you give it:

```
$ rea new mkvize 'ffmpeg -i {1} -y -c:v copy -c:a copy {1}.mkv'
$ mkvize 'Holiday 2024.mp4'          # writes 'Holiday 2024.mp4.mkv'
```

Quote the template with single quotes so your shell passes it to rea
untouched.

Add a check after a colon to make sure an argument is what you expect.
`F` means "must be an existing file", `D` an existing directory, `E` must
exist, `N` must not exist yet:

```
$ rea new odt2md 'pandoc {1:F} -t gfm -o {2:N} --wrap=none'
$ odt2md notes.odt notes.md
```

If `notes.odt` is missing or `notes.md` already exists, `odt2md` prints why
and exits with status 2 before `pandoc` starts.

Modifiers after the colon take paths apart. With `dir/photo.jpg` as `{1}`:
`{1:/}` is `photo.jpg`, `{1://}` is `dir`, `{1:.}` is `dir/photo`, and
`{1:/.}` is `photo`.

```
$ rea new silounzip 'unzip {1:F} -d {1:/.}{:N}'
$ silounzip ~/Downloads/fonts.zip     # unzips into ./fonts, if it doesn't exist yet
```

`{:N}` at the end of a word checks the whole word, here the `fonts`
directory, rather than the argument.

To work with any number of arguments, use `{@+}` (one or more) or `{@}`
(zero or more). A word containing them is repeated once per argument:

```
$ rea new mkexe 'chmod +x {@+:F}'
$ mkexe build.sh deploy.sh
```

`{-1}` is the last argument, and `{..-1}` is every argument but the last.
That is how `mvt` at the top of this page moves any number of files into a
directory given last.

A command with no `{...}` at all works like a classic shell alias, with the
arguments added at the end:

```
$ rea new fgrep 'grep -F'
$ fgrep -r 'a.b' src/                 # runs: grep -F -r a.b src/
```

## Keeping aliases in a file

Once you have more than a few aliases, keep them in a text file (in your
dotfiles, say), one `name = template` per line, with `#` comments:

```sh
# ~/dotfiles/aliases.txt

# File moves
cpt = cp -r -t {-1:D} -- {..-1:E}
mvt = mv -t {-1:D} -- {..-1:E}

# Documents
odt2md = pandoc {1:F} -t gfm -o {2:N} --wrap=none
silounzip = unzip {1:F} -d {1:/.}{:N}

# Shortcuts
fgrep = grep -F
showpath = printenv PATH | tr ":" "\n"
```

Then create or update all of them at once:

```sh
rea patch ~/dotfiles/aliases.txt              # create or update them all
rea patch ~/dotfiles/aliases.txt --dry-run    # show what would change
rea patch ~/dotfiles/aliases.txt --prune      # also remove aliases no longer in the file
rea patch ~/dotfiles/aliases.txt mvt cpt      # only these two
rea check ~/dotfiles/aliases.txt              # validate without writing anything
```

`patch` checks the whole file first and reports every problem at once, with a
caret under each mistake, before it writes anything. It only rewrites aliases
whose definition changed, and it never overwrites a file that isn't a rea
alias unless you pass `--replace-non-aliases`.

Going the other way, `rea save ~/dotfiles/aliases.txt` copies aliases you
created with `rea new` back into the file. Existing lines keep their place and
comments; new aliases are appended under a `# added by rea save` comment for
you to move where they belong. `rea list` prints every alias in a directory
in the same file format.

## Seeing what an alias will do

`rea explain` expands an alias with the arguments you give it and prints the
commands it would run, without running them. Each argument appears exactly
as the program will receive it, quoted where needed, along with any
environment variables the alias reads:

```
$ rea explain jpg-frames ../my-vid.mp4 2
   mkdir -p my-vid
&& ffmpeg -i ../my-vid.mp4 -q:v 2 my-vid/my-vid_frame_%04d.ppm
&& fd -0 -t f -g '*_frame_*.ppm' my-vid
  | xargs -0 -n 1 -P 0 convert-jpg
&& du -hs my-vid
(from the environment: INTER_FORMAT=ppm)
```

Here you can check that `fd` receives the glob `*_frame_*.ppm` as a single
argument, that the frames land in the directory you expect, and which
`INTER_FORMAT` was used. When an alias misbehaves, this is the first thing to
run.

`explain` also lists checks that would fail, after the commands, so you can
debug an alias without first creating or deleting files to satisfy it:

```
$ rea explain mvt photo.png nowhere
   mv -t nowhere -- photo.png
(single command: runs via exec, no supervisor process)
rea: would not run: {-1:D} needs an existing directory, but 'nowhere' does not exist
```

Other tools for working on an alias:

```sh
rea inspect mvt                        # print the template stored in an alias
rea explain -t 'cp {1} {2:D}' a b      # explain a template before making it an alias
rea eval 'echo {1:/.} {1://}' a/b.txt  # run a template once, to try it out
```

Mistakes in a template are reported when you create the alias, pointing at
the problem:

```
$ rea new cp3 'cp {1} {3} {@}'
rea: alias `cp3`: `{3}` is used but `{2}` is not; every argument must be used, so number them without gaps (or cover the gap with a slice)
    cp {1} {3} {@}
           ^
```

## Template syntax

`rea --help` prints this table too.

### Arguments

| Syntax | Meaning |
|---|---|
| `{1}`, `{2}`, ... | arguments counted from the front; can be glued to text: `{1}.mkv` |
| `{-1}`, `{-2}`, ... | arguments counted from the back: `{-1}` is the last |
| `{@}` | every argument not used elsewhere, zero or more |
| `{@+}` | the same, but at least one is required |
| `{A..B}` | a slice: from argument A up to but **not including** B. `{2..}` is the 2nd onward, `{..-1}` all but the last |
| `{0}` | the alias's own name |
| `{$NAME}` | the environment variable `NAME`, read when the alias runs |

### Checks

Checks run before any command starts. If one fails, the alias prints why and
exits with status 2.

| Syntax | The argument must... |
|---|---|
| `{1:F}` | be an existing file (anything but a directory) |
| `{1:D}` | be an existing directory |
| `{1:E}` | exist |
| `{1:N}` | not exist yet |
| `word{:N}` | (at the end of a word) the same checks, applied to the whole word: `{1}.jpg{:N}` |

### Path modifiers

| Syntax | With `dir/photo.jpg` |
|---|---|
| `{1:/}` | `photo.jpg` (basename) |
| `{1://}` | `dir` (parent directory) |
| `{1:.}` | `dir/photo` (without extension) |
| `{1:/.}` | `photo` (basename without extension) |

Checks and modifiers work on every kind of reference except `{0}`, and
combine: `{1:F/.}` requires an existing file and gives its basename without
the extension. `{@+:F}` checks every argument.

### Commands and quoting

| Syntax | Meaning |
|---|---|
| `a \| b` | pipe |
| `a && b`, `a \|\| b`, `a ; b` | run in sequence, as in sh |
| `'...'` | fully literal |
| `"..."` | literal except `{...}` references and the escapes `\" \\ \{ \} \$ \`` |
| `\x` | literal `x` |

There is no shell, so `$VAR`, backticks, globs (`*`), redirection (`>`),
subshells and `&` are errors rather than being passed through by surprise.
Quote or escape them to pass them literally, as in `fd -g \*.jpg`. Use
`{$VAR}` for environment variables. For anything a template can't express,
call `sh -c` from the template.

## Examples

**Copy or move several files into a directory** given last, with each
source checked:

```
cpt = cp -r -t {-1:D} -- {..-1:E}
mvt = mv -t {-1:D} -- {..-1:E}
```

**Symlink files into a directory**, using absolute paths:

```
symlink-into = realpath -z -- "{@+:E}" "{-1:D}" | xargs -0 -r ln -s --
```

**Convert an image to JPEG** and remove the original, but only if the
conversion succeeded and the output doesn't already exist:

```
convert-jpg = magick {1:F} ppm: | cjpeg -quality 85 -outfile {1}.jpg{:N} && rm {1} && mv {1}.jpg {1:.}.jpg
```

If `magick` or `cjpeg` fails, the pipeline fails and `rm` never runs.

**Aliases calling aliases.** Because aliases are programs, `xargs` can run
them, and one alias can be built from others. `chext` changes the extension
of any number of files, using three small helpers:

```
_nul-sep = printf "%s\0" {@+}
_nul-sep-runner = xargs -0 -n1 -- {@+}
_chext = mv {2:F} {2:.}.{1}{:N}
chext = _nul-sep {@+} | _nul-sep-runner _chext {-1:N}
```

```
$ chext *.jpeg jpg
```

**Checks that catch mistakes.** The `{-1:N}` in `chext` guards against
forgetting the extension. The last argument is meant to be an extension, so
it should never name an existing file. Without the check, `chext *` in a
directory holding `a`, `b` and `c` would quietly rename `a` and `b` to
`a.c` and `b.c`. With it, nothing runs:

```
$ chext *
chext: {-1:N} needs a path that doesn't exist yet, but 'c' already exists
    chext = _nul-sep {@+} | _nul-sep-runner _chext {-1:N}
$ chext * txt
$ ls
a.txt  b.txt  c.txt
```

When you write an alias, ask what a slip of the fingers or an unexpected glob
would do with it, and add the check that rules that out.

**`rea eval` in place of `sh -c` for xargs.** To run a short command per
item, use `rea eval` with a template as the lambda. Each item is one word,
whatever it contains, and the template's checks apply to every item:

```
$ fd -0 -e flac | xargs -0 -n1 -P0 rea eval 'ffmpeg -i {1:F} {1:.}.opus{:N}'
```

Files whose `.opus` already exists are skipped with a message, and the
others are converted. Compare
`xargs -0 -I{} sh -c 'ffmpeg -i "{}" …'`, which breaks on a filename with a
quote in it. Inside an alias, escape the inner template's braces so the
outer alias leaves them for `rea eval`:

```
to-opus = fd -0 -e flac . {@:D} | xargs -0 -n1 -P0 rea eval "ffmpeg -i \{1:F\} \{1:.\}.opus\{:N\}"
```

`rea explain to-opus music` shows the inner template as `rea eval` will
receive it.

**`rea eval` in scripts and Makefiles.** The same idea works anywhere a
one-off command needs path manipulation or a safety check. This loop from
`bench/Makefile` renames `x-short.perf.json` to `x-short-no-pie.perf.json`
in two steps, and stops `make` rather than overwriting anything:

```make
no-pie:
	for i in *.json; do rea eval 'mv {1} {1:.}{:N}' "$$i" || exit; done
	for i in *.perf; do rea eval 'mv {1} {1:.}-no-pie.perf.json{:N}' "$$i" || exit; done
```

In sh, this would take `"${i%.json}"` for the first rename and a separate
`[ -e … ]` test for each target. `mv -n` would skip an existing target
without failing.

**Extract video frames as JPEGs in parallel**, with the intermediate format
taken from the environment:

```
jpg-frames = mkdir -p {1:/.} && ffmpeg -i {1:F} -q:v {2} {1:/.}/{1:/.}_frame_%04d.{$INTER_FORMAT} && fd -0 -t f -g \*_frame_\*.{$INTER_FORMAT} {1:/.} | xargs -0 -n 1 -P 0 convert-jpg && du -hs {1:/.}
```

**Read a value from a file** with an inline script. References work inside
double quotes:

```
cargo-ver = python3 -c "import tomllib; print(tomllib.load(open('{1:F}', 'rb'))['workspace']['package']['version'])"
```

**Use environment variables** with `{$NAME}`. An unset or empty variable is
an error before anything runs:

```
scc = docker run --rm -it -v "{$PWD}:/pwd:ro" --network none ghcr.io/boyter/scc:master scc /pwd {@}
```

**Pipelines with no arguments:**

```
showpath = printenv PATH | tr ":" "\n"
git-branch-cleanup = git for-each-ref --format='%(refname:short) %(upstream:track)' refs/heads | awk '$2 == "[gone]" { print $1 }' | xargs -r git branch -d
```

**Optional arguments** with `{@}`:

```
penv = printenv {@} | sort            # all variables, or just the ones named
```

## Command reference

| Command | What it does |
|---|---|
| `rea new NAME TEMPLATE` | create or update one alias |
| `rea patch FILE [NAME...]` | create or update aliases from an alias file (`--prune`, `--dry-run`) |
| `rea check FILE` | validate an alias file without writing anything |
| `rea save FILE [DIR]` | update an alias file from the aliases in a directory (`--dry-run`) |
| `rea list [DIR]` | print the aliases in a directory, as an alias file |
| `rea inspect ALIAS...` | print the template stored in aliases |
| `rea explain ALIAS ARGS...` | print what an alias would run, without running it |
| `rea explain -t TEMPLATE ARGS...` | the same, for a template |
| `rea eval TEMPLATE ARGS...` | run a template once, without creating an alias |
| `rea eval - ARGS...` | the same, reading the template from stdin |
| `rea completions bash\|zsh\|fish` | print a tab-completion script |

`rea COMMAND --help` describes each one.

- **Where aliases go.** `new`, `patch`, `list` and `save` use the directory
  `rea` is installed in, unless `REA_ALIAS_DIR` is set or you pass `-o DIR`
  (or a DIR argument).
- **Finding an alias.** For `inspect` and `explain`, ALIAS is a path or a bare
  name. A bare name is looked up on `$PATH` like a command, then in the alias
  directory, so `rea explain mvt` reads the same file running `mvt` would.
- **`explain`'s exit status** matches the alias's: 2 for a failed check, an
  unset variable, a wrong argument count or an invalid template.
- **`eval -`** reads one line from stdin as the template, which avoids a layer
  of shell quoting (`rea eval - < template.txt`, or a heredoc). The template's
  own commands then see stdin at end of file.
- **`list` and `patch` round-trip.** `rea list DIR > aliases.txt` followed by
  `rea patch aliases.txt` recreates the same aliases.
- **`save`** keeps comments, blank lines, order and entries with no matching
  alias. It follows a symlinked file and rewrites a hard-linked one in place
  (with a backup beside it until the write finishes), so links survive.

## How aliases run

- **A single command** replaces the alias process (`exec`), so its PID,
  signals and exit status are the command's own.
- **Pipelines and lists** (`|`, `&&`, `||`, `;`) run under a small supervisor
  that starts each command directly and connects the pipes:
  - A pipeline fails if any stage fails, like bash's `set -o pipefail`. The
    exception is an earlier stage killed by SIGPIPE, as in `yes | head -1`,
    which counts as success.
  - A missing command exits 127, any other failure to start one exits 126.
  - Ctrl-C reaches every stage, and the supervisor waits for them to clean up.
    The rest of the list is then skipped and the alias exits by SIGINT, so a
    calling script stops as it would under bash.
  - When started with SIGINT ignored (a background job, or under `nohup`), the
    stages inherit that, as they would from a shell.
- **All checks run up front**, before the first command starts. A file that an
  earlier step in the alias creates therefore doesn't pass a later check on
  it.

## Template rules in detail

These rules exist to catch mistakes. They rarely get in the way of a correct
template.

- **One word per reference.** `{1}`, `{-1}` and `{$NAME}` are substituted
  straight into the command's arguments, so a value with spaces stays one
  word. No quoting is ever needed around them.
- **Repeating words.** A word containing `{@}`, `{@+}` or a slice is repeated
  once per argument: `-I{@}` gives `-Ia -Ib`, and nothing at all when there
  are none. So `rm -rf /{@+}` can never become `rm -rf /`. The same reference
  may appear twice in a word (`{@}={@}` gives `a=a b=b`), but two different
  ones may not.
- **Every argument must be used.** `{N}` and `{-N}` numbering has no gaps:
  `{3}` needs `{2}`, or a slice that covers it (`{1} {2..} {3}` is fine).
  `{@}` doesn't fill gaps, so the typo `cp {1} {3} {@}` is still caught.
  Numbers go up to 5000.
- **The argument count is exact** unless `{@}`, `{@+}` or a slice is used.
  `mkvize *.mp4` with three files is an error, rather than quietly
  processing the first one.
- **Slices** may overlap other references on purpose (`{1} {1..}`), and are
  never empty: `rm {2..}` needs a second argument. `{@}` never repeats what a
  slice already passed: in `{1} {2..-2} {@}` with five arguments, `{@}` is the
  4th and 5th.
- **`{@}` with slices** must be one run of arguments at the start or end, so a
  slice can't split it (`{2..-2} {@}`) or leave a gap for it
  (`{-2..} {..-3} {@}`, a typical off-by-one). Without slices it can sit in
  the middle: `{1} --opt val {@} -o {-1}`.
- **Ambiguous counts are refused.** If a slice would overlap the other
  references differently at some argument count, that count is rejected:
  `{1..4} {-1}` takes exactly 4, since with 3 the slice would include the
  last argument. A template that no count satisfies is refused when the alias
  is created.
- **Checks are about the argument**, wherever the reference appears:
  `{1:N}.jpg` checks argument 1, not `….jpg`. To check the path a word builds,
  end the word with a check of its own: `-outfile {1}.jpg{:N}`. That check
  covers the whole word, literal text included, so use it on words that are
  only a path (`--out={1}{:N}` would check `--out=…`).
- **Contradictory checks** on the same argument, like `{1:F}` and `{1:N}`,
  are refused when the alias is created.
- **Check details.** Checks follow symlinks, except `N`, for which a dangling
  symlink counts as existing. `F` passes anything that isn't a directory, so
  `/dev/null`, named pipes and bash's `<(...)` work as files.
- **Modifiers are textual**, as in `fd`: `a.tar.gz` loses only `.gz`,
  `.bashrc` has no extension, and a bare name's parent is `.`.
- **Environment variables** are one word, never split. So `CC="zig c++"`
  names a program called `zig c++`. For a command with arguments, make a
  wrapper alias (`rea new zigcxx 'zig c++'`) and point the variable at it.
- **No references at all.** A single command with no `{...}` (not even `{0}`
  or `{$NAME}`) gets `{@}` appended, like a shell alias.
- **Shell syntax is refused.** `$` outside `{$NAME}`, backticks, globs,
  redirection, `( )` and a lone `&` are errors. `$` and backticks need `\$`
  and `` \` `` even inside double quotes, as in sh, and
  `find -exec ... '{}' \;` needs the braces and `;` quoted, also as in sh.

## Installation details

### Make targets and variables

`make help` lists every target. `make aliases` runs `rea patch` on an alias
file. Two variables change where things go:

- `PREFIX` (default `~/.local`): binaries go in `$PREFIX/bin`.
- `ALIASES` (default the example `aliases.txt` in this repository): the alias
  file `make aliases` uses, e.g. `make aliases ALIASES=~/dotfiles/aliases.txt`.

### Alias directory

`rea` looks for `rea-stub` next to itself and by default creates aliases in
its own directory. `--stub PATH` and `-o DIR` override both.

`REA_ALIAS_DIR` changes the default alias directory for `new`, `patch`,
`list` and `save`, and is where `explain` and `inspect` look last for a bare
name. `-o DIR` still overrides it, and an empty value counts as unset.

From a clone, `make install` builds the same binaries as `cargo install rea`,
installing them into `~/.local/bin` (see `PREFIX` above). Without make, that
is `cargo install --locked --path crates/rea --root ~/.local`; `--locked`
makes cargo use `Cargo.lock`, which `cargo install` otherwise ignores.

### Tab completion

```sh
# bash
rea completions bash > ~/.local/share/bash-completion/completions/rea
# zsh (with fpath+=~/.zfunc before compinit in ~/.zshrc)
rea completions zsh > ~/.zfunc/_rea
# fish
rea completions fish > ~/.config/fish/completions/rea.fish
```

The script is generated for the name rea runs as, so a renamed copy or a
symlink completes under its own name; name the file after it too. An alias
for rea (`rea new r rea`) runs it as `rea` and gets the `rea` script; in
bash, `complete -F _rea r` after loading it covers the alias too. Regenerate
the script after upgrading.

### Static builds and other machines

`make install-static` builds against musl, giving fully static binaries that
run on any Linux machine of the same architecture with no glibc version to
match. It needs the target once:
`rustup target add x86_64-unknown-linux-musl` (`make install-static` prints
the exact command if it's missing). Plain `make install` links glibc
dynamically, so its binaries need a compatible glibc wherever they go.

To install on a machine without Rust, copy the two binaries from a static
build: they are all rea needs. Aliases themselves need nothing else at run
time and can be copied anywhere, under any name.

`PIE=no` (e.g. `make install-static PIE=no`) builds binaries that aren't
position-independent, which removes a good part of each alias's remaining
startup cost (on x86_64 musl, about 7k of 12k user instructions, plus about
25k kernel ones). The trade-off is that the binary's own code always loads
at the same address; the stack, heap and other mappings are still
randomized. Switching `PIE` rebuilds everything, and a plain
`make build PIE=no` puts its binaries in `target/<host triple>/release`.

### Upgrading and uninstalling

Each alias contains a copy of the stub, so after upgrading rea, run
`rea patch` on your alias file to move every alias onto the new stub:

```sh
cargo install rea && rea patch ~/dotfiles/aliases.txt
```

From a clone, `make install aliases` does both. Aliases are listed as
"updated … (stub only)" unless their definition changed too. Aliases you
don't re-patch keep working on the old stub.

`make uninstall` removes `rea` and `rea-stub` and leaves your aliases, which
keep working on their own.

**From aliasmux** (rea's earlier name): `make install aliases` installs rea
and rewrites the aliases in your alias file onto `rea-stub`. rea recognizes
aliasmux aliases, so replacing them needs no `--replace-non-aliases`, and
any it doesn't rewrite keep working. Remove the old binaries with
`cargo uninstall --root ~/.local aliasmux`.

## Migrating from alias_maker.py

rea succeeds `alias_maker.py` (2012). In its `bash -c` aliases, `$0` was the
first argument, so `$0` becomes `{1}`, `$1` becomes `{2}`, and `"$@"` becomes
`{2..}`. Drop the `bash -c '...'` wrapper and the shell quoting around `$0`.
A leftover `$0` is reported as an error, so nothing slips through
unconverted.

## How it works

Each alias is a copy of the `rea-stub` executable with the alias's name and
template written into it. Creating one needs no compiler, and running one
parses the template with the same code `rea` used to validate it.

- `crates/core`: the runtime: template language, alias file format, process
  execution, and the slot format that holds an alias inside its stub.
- `crates/rea`: the `rea` tool (`src/main.rs`, `src/cli.rs`) and the stub
  (`src/bin/rea-stub.rs`), built side by side.

Earlier designs (a multicall binary, a shared-library variant with a C stub,
and separately compiled per-alias binaries) are in the git history.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.