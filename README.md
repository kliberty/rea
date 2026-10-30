# rea

Command aliases with numbered arguments and real pipelines, and no shell.

The name is from *mens rea*, and is also an alternate spelling of Rhea.

```sh
$ rea new tojpg 'convert {1:F} {1:.}.jpg{:N}'
created /home/me/.cargo/bin/tojpg

$ tojpg holiday.png          # runs: convert holiday.png holiday.jpg
$ tojpg holiday.png
tojpg: the word {1:.}.jpg{:N} needs a path that doesn't exist yet, but 'holiday.jpg' already exists
    tojpg = convert {1:F} {1:.}.jpg{:N}
```

Each alias is a small standalone executable, so it works anywhere a program does. You can call it from any shell (bash, zsh, fish, PowerShell…), from scripts and Makefiles, from `xargs` and `find -exec`, and from other aliases. No shell is ever involved when it runs. Arguments are never word-split or globbed, a file name with spaces stays one argument, and the template can say up front which arguments must be existing files, directories, or paths that don't exist yet. If a check fails, nothing runs.

## Contents

- [Installing](#installing)
- [Quick start](#quick-start)
- [Templates](#templates)
  - [Arguments](#arguments)
  - [How many arguments an alias takes](#how-many-arguments-an-alias-takes)
  - [Path checks and modifiers](#path-checks-and-modifiers)
  - [Environment variables](#environment-variables)
  - [Pipes and command lists](#pipes-and-command-lists)
  - [Quoting](#quoting)
  - [What is deliberately not supported](#what-is-deliberately-not-supported)
- [Trying things out: `eval` and `explain`](#trying-things-out-eval-and-explain)
- [Alias files: `check`, `patch`, `save`, `list`](#alias-files)
- [Recipes](#recipes)
- [Errors and exit status](#errors-and-exit-status)
- [How aliases run](#how-aliases-run)
- [Command reference](#command-reference)
- [Gotchas](#gotchas)

## Installing

Install from crates.io with Cargo:

```sh
cargo install rea
```

The source is at <https://github.com/kliberty/rea>.

This installs two executables into Cargo's bin directory (`~/.cargo/bin`), which is normally already on your `PATH`:

| File       | What it is                                                     |
|------------|----------------------------------------------------------------|
| `rea`      | The tool that creates, inspects and manages aliases             |
| `rea-stub` | The template every alias is copied from. Don't run it directly. |

Keep them in the same directory. rea looks for `rea-stub` next to itself.

**Platforms.** rea is developed on Linux. It builds and runs on FreeBSD, but has only been tested there lightly. ARM targets haven't been tried yet.

**Where aliases go.** By default rea writes aliases into the directory `rea` itself lives in, so they're on your `PATH` straight away. To keep them somewhere else, set `REA_ALIAS_DIR`, or pass `-o DIR` to commands that write. Whatever directory you use must be on your `PATH`. A missing directory is created, and an empty `REA_ALIAS_DIR` counts as unset.

With a Cargo install, the default is `~/.cargo/bin`. Mixing aliases in with installed tools is safe, because rea can always tell them apart. This removes every alias from a directory and leaves everything else alone:

```sh
rea patch --prune <(echo)          # prune against an empty alias file; add --dry-run to preview
```

If you'd rather keep them separate, use a directory of their own:

```sh
export REA_ALIAS_DIR=~/.local/share/rea/bin
export PATH="$REA_ALIAS_DIR:$PATH"
```

**Tab completion** (bash, zsh, fish, elvish, PowerShell):

```sh
rea completions bash > ~/.local/share/bash-completion/completions/rea
rea completions zsh  > ~/.zfunc/_rea        # with fpath+=~/.zfunc before compinit
rea completions fish > ~/.config/fish/completions/rea.fish
```

## Quick start

```sh
# Create an alias. Quote the template so your shell leaves it alone.
$ rea new greet 'echo hello {1}'
created /home/me/.cargo/bin/greet

$ greet "big world"
hello big world

# The argument count is enforced.
$ greet
greet: expected exactly 1 argument, got 0
    greet = echo hello {1}

# Re-running `new` updates the alias in place. An identical definition isn't rewritten.
$ rea new greet 'echo hi {1}'
updated /home/me/.cargo/bin/greet
$ rea new greet 'echo hi {1}'
unchanged /home/me/.cargo/bin/greet

# A template with no references at all works like a shell alias: arguments go on the end.
$ rea new g 'git'
$ g status -s                # runs: git status -s

# See what you have.
$ rea list
g = git
greet = echo hi {1}
```

## Templates

A template is one or more commands, written much like a shell command line, with `{…}` references where the alias's arguments go.

### Arguments

| Reference  | Meaning                                                                         |
|------------|---------------------------------------------------------------------------------|
| `{1}` `{2}` … | Arguments counted from the front                                             |
| `{-1}` `{-2}` … | Arguments counted from the back. `{-1}` is the last.                        |
| `{@}`      | Every argument that no other reference uses. Zero or more.                      |
| `{@+}`     | Same as `{@}`, but at least one is required                                     |
| `{A..B}`   | A slice, from argument A up to but **not including** B. Ends are counted like `{N}`/`{-N}` and either may be left out: `{2..}` is the 2nd onward, `{..-1}` is all but the last. Slices are never empty. |
| `{0}`      | The alias's name                                                                |

A single reference like `{1}` is always exactly one argument, spaces and all. Nothing is ever split.

A word that contains `{@}`, `{@+}` or a slice is repeated once per argument:

```sh
$ rea eval 'echo -I{@}' a b
-Ia -Ib
$ rea eval 'echo -I{@}'          # no arguments: the word disappears entirely

$ rea explain -t 'tar -czf {1:N} {2..}' out.tgz a.png b.png
   tar -czf out.tgz a.png b.png
$ rea explain -t 'cp -v {..-1:F} {-1:D}' a.png b.png dest
   cp -v a.png b.png dest
```

(`rea eval` runs a template without creating an alias. `rea explain` prints what it would run. See [below](#trying-things-out-eval-and-explain).)

### How many arguments an alias takes

rea works out the allowed argument count from the template and refuses anything else:

- **Every argument must be used.** An alias can never accept an argument and then ignore it. Numbering can't have gaps: `{3}` needs `{2}` (or a slice covering it). A template like `echo {2}` is rejected when you create it, not when you run it.
- **Without `{@}`, `{@+}` or a slice, the count is exact.** So `echo {1} {-1}` takes exactly two arguments. To take a first and last argument with anything in between, add `{@}`: `echo {1} {@} {-1}` takes two or more.
- **Slices and `{@+}` set a minimum.** `{2..}` needs at least two arguments.
- **With slices, `{@}` must be one run at the start or the end**, so it's always clear which arguments it gets.

```sh
$ rea new bad 'echo {2}'
rea: alias `bad`: `{2}` is used but `{1}` is not; every argument must be used, so number them without gaps (or cover the gap with a slice)
    echo {2}
         ^
```

Numbers go up to 5000.

### Path checks and modifiers

Add a colon after any reference except `{0}` to check the argument against the file system, edit its text, or both.

**Modifiers** edit the text only:

| Modifier | Result for `dir/photo.jpg` |
|----------|----------------------------|
| `{1:/}`  | `photo.jpg` (basename)     |
| `{1://}` | `dir` (parent directory)   |
| `{1:.}`  | `dir/photo` (no extension) |
| `{1:/.}` | `photo` (basename, no extension) |

**Checks** make the alias refuse to run unless the argument passes:

| Check   | The argument must…                                                     |
|---------|------------------------------------------------------------------------|
| `{1:F}` | be an existing file. Anything that isn't a directory counts, so `/dev/null` and `<(…)` pass. |
| `{1:D}` | be an existing directory                                               |
| `{1:E}` | exist                                                                  |
| `{1:N}` | **not** exist yet                                                      |

Combine them with the check first: `{1:F/.}` requires an existing file and gives its basename without the extension. **The check is always on the original argument**, before the modifier changes it.

To check the *finished word* rather than the argument, put the check in braces at the very end of the word:

```sh
tojpg = convert {1:F} {1:.}.jpg{:N}
#                              ^^^^ checks "photo.jpg", not "photo.png"
```

On a repeated word (`{@}.bak{:N}`), each copy is checked.

All checks run before anything starts. So a file that an earlier command in the same alias would create doesn't count as existing yet.

### Environment variables

`{$NAME}` inserts an environment variable as exactly one argument, never split. If it's unset **or empty**, the alias refuses to run:

```sh
$ rea new edit '{$EDITOR} {1}'
$ EDITOR= edit notes.txt
edit: {$EDITOR} is set but empty
    edit = {$EDITOR} {1}
```

A bare `$` anywhere else is an error. See [What is deliberately not supported](#what-is-deliberately-not-supported).

### Pipes and command lists

| Syntax     | Meaning                              |
|------------|--------------------------------------|
| `a \| b`   | pipe a's output into b               |
| `a && b`   | run b only if a succeeded            |
| `a \|\| b` | run b only if a failed               |
| `a ; b`    | run b regardless                     |

`&&`, `||` and `;` have equal precedence and group left to right, as in sh. There are no parentheses for grouping.

```sh
$ rea new words 'printf "%s\n" {@} | sort | uniq -c'
$ words b a b
      1 a
      2 b
```

### Quoting

Quoting works as in sh:

- `'…'` is completely literal.
- `"…"` is literal except for `{…}` references and the escapes `\"` `\\` `\{` `\}` `\$` `` \` ``.
- `\x` outside quotes is a literal `x`.

So to pass braces through literally, quote or escape them. rea catches the common case:

```sh
$ rea eval 'find . -name {1} -exec echo {} \;' x
rea: invalid template: `{}` is not an argument reference; quote it ('{}') to pass literal braces, e.g. for find -exec

$ rea eval "find . -name {1} -exec echo '{}' ';'" '*.png'      # works
```

Remember there are two layers of quoting: your shell's, around the whole template, and the template's own, inside it. Single-quoting the template on the command line is easiest. In an [alias file](#alias-files) there's only the template's layer.

### What is deliberately not supported

rea doesn't run a shell, so shell features that would normally be expanded are **errors**, not silently passed through:

| You wrote          | Why it's rejected                                 | Instead                                  |
|--------------------|---------------------------------------------------|------------------------------------------|
| `$HOME`, `$1`      | No variable expansion                             | `{$HOME}`, `{1}`, or `\$` for a literal `$` |
| `*.txt`, `?`, `[…]`| No globbing                                       | Let your shell expand globs into the arguments: `myalias *.txt` with `{@}` |
| `> file`, `< file`, `2>&1` | No redirection                            | Use a command that takes a file name (`tee`, `-o FILE`), or redirect when you call the alias |
| `` `cmd` ``, `(…)` | No subshells or command substitution              | Use a script                             |
| `cmd &`            | No background jobs                                | `&&` if that's what you meant            |

Each of these gives an error pointing at the offending character. Quote or escape the character if you really want it passed literally.

Shell **builtins** don't exist either, because there's no shell. `cd`, `export`, `source`, `alias` and so on fail with `command not found` (exit 127). An alias runs in its own process, so it couldn't change your shell's directory or environment anyway.

## Trying things out: `eval` and `explain`

**`rea eval TEMPLATE [ARGS…]`** runs a template once, without creating anything. It's the quickest way to experiment. Inside `eval`, `{0}` is `rea`.

```sh
$ rea eval 'echo first={1} last={-1} middle={2..-1}' a b c d
first=a last=d middle=b middle=c
```

With `-` as the template, the template is read from one line of stdin. The template's commands then see stdin already at end-of-file.

**`rea explain ALIAS [ARGS…]`** prints the commands an alias would run with those arguments, quoted so you could paste them into sh, and runs nothing. Any checks that would fail are listed afterwards. The exit status is what the alias would return for an argument or check error (2), so it's scriptable. Add `-t` to explain a template instead of an alias.

```sh
$ rea explain words b a b
   printf '%s\n' b a b
  | sort
  | uniq -c

$ rea explain tojpg missing.png
   convert missing.png missing.jpg
(single command: runs via exec, no supervisor process)
rea: would not run: {1:F} needs an existing file, but 'missing.png' does not exist
```

**`rea inspect ALIAS…`** prints the stored definition of one or more aliases, in alias-file format.

`explain` and `inspect` accept a path, or a bare name that's looked up on `PATH` and then in the default alias directory.

> ⚠️ `eval` and `explain` recognise their own options anywhere on the line. If an argument meant for the template is `-h`/`--help`, rea prints its help instead. For `explain`, an argument of `-t`/`--template` quietly switches it to template mode, so `rea explain greet -t` explains the one-word template `greet`. Put `--` before the arguments to be safe: `rea explain greet -- --help`. Other dash arguments pass through as they are, and real aliases are never affected: `greet --help` prints `hello --help`.

## Alias files

To keep a set of aliases in version control, or carry them between machines, put them in an alias file. Use one `name = template` per line. Blank lines are ignored, and lines starting with `#` are comments.

```ini
# media
tojpg = convert {1:F} {1:.}.jpg{:N}
mkv   = ffmpeg -i {1:F} -c copy {1:/.}.mkv{:N}

# git
gl  = git log --oneline --graph -n {1}
gco = git switch {@+}
words = printf "%s\n" {@} | sort | uniq -c
```

The line is split at the first `=`, so templates can contain `=` (`kv = env FOO=bar {@}`). Spaces around the first `=` are optional.

**Names** may contain letters, digits, `.`, `_`, `+` and `-`, but can't start with `.` or `-`. The name is also the executable's file name.

### `rea check FILE`

`check` validates every line and writes nothing. It reports **all** problems at once, with line numbers:

```
$ rea check bad.txt
rea: bad.txt:3: alias `dup` is already defined on line 2
rea: bad.txt:4: expected `name = template`
rea: bad.txt:6: alias `broken`: `{2}` is used but `{1}` is not; ...
    echo {2}
         ^
```

### `rea patch FILE [NAMES…]`

`patch` creates or updates the aliases a file defines, or just the ones named. The whole file is validated first. If any line is invalid, nothing is written.

```sh
rea patch aliases.txt              # all of them
rea patch aliases.txt gl gco       # just these
rea patch --dry-run aliases.txt    # show what would happen
rea patch --prune aliases.txt      # also delete aliases the file no longer defines
```

Each alias is reported as `created`, `updated`, `replaced`, `unchanged`, `removed` or `failed`, with a summary line at the end. Unchanged aliases aren't touched. `--prune` only ever removes rea aliases. Other programs in the directory are left alone.

### `rea save FILE [DIR]`

`save` is the reverse of `patch`: it updates an alias file from the aliases that actually exist in a directory. Use it after changing aliases with `rea new`. The file is edited conservatively:

- An alias the file already has keeps its line, its position and its spacing. Only the template part is replaced, and only if it changed.
- Aliases the file lacks are appended in name order, under a `# added by rea save` comment.
- Comments, blank lines, and entries with no matching alias in the directory are kept.
- An unchanged file isn't rewritten, a file that doesn't parse is refused, and a missing file is created.

`--dry-run` shows what would change.

### `rea list [DIR]`

`list` prints every alias in a directory as a valid alias file, in name order. So this recreates a directory's aliases somewhere else:

```sh
rea list > aliases.txt
rea patch -o ~/other/bin aliases.txt
```

## Recipes

### Keep your alias file in sync with aliases

Aliases can manage aliases. With these two, `rea new` changes are one command away from your file, and a new machine is one command away from your aliases:

```ini
rea-backup = rea save {$HOME}/aliases.txt
rea-deploy = rea patch {$HOME}/aliases.txt
```

### Build bigger aliases from helpers

An alias can call other aliases, in pipelines and lists like any other command. A leading `_` is a handy convention for helpers that aren't meant to be called directly. Here is a complete "change extension" command built from small, separately testable pieces:

```ini
# helpers
_nul-sep = printf "%s\0" {@+}
_die     = perl -e 'die "@ARGV\n"' {@+}
_ext-ok  = _nul-sep {2} | grep -qazP '^\.\w+\z' || _die {1}: extension \"{2}\" must look like .txt
_chext   = mv -n -- {2:F} {2:.}{1}{:N}

# chext FILE... .EXT
chext = _ext-ok {0} {-1} && _nul-sep {@+:F} | xargs -0 -n1 -- _chext {-1}
```

```
$ chext a.jpg b.jpg txt
chext: extension "txt" must look like .txt
$ chext a.jpg b.jpg .png
_chext: the word {2:.}{1}{:N} needs a path that doesn't exist yet, but 'b.png' already exists
    _chext = mv -n -- {2:F} {2:.}{1}{:N}
```

Some things to notice:

- `_ext-ok {0} …` passes the caller's name down, so the error names `chext` and not the helper.
- `_die` stands in for the `echo …; exit 1` you'd write in a script.
- `{@+:F}` checks every file before anything is renamed. Each file is then handed to `_chext`, which does its own checks, one file at a time.
- `xargs` runs `_chext` once per file, so one failure doesn't stop the rest. In the example above, `a.jpg` was still renamed. `xargs` exits 123 if any run failed.

### Check every output name before doing anything

In the version above, a clash is only discovered when its turn comes. To refuse the whole job up front, put the derived names on a command that ignores its arguments. `true` does nothing, but the checks on its arguments still run before anything starts:

```ini
chext-strict = _ext-ok {0} {-1} && true {@+:.}{-1}{:N} && _nul-sep {@+:F} | xargs -0 -n1 -- _chext {-1}
```

```
$ chext-strict a.jpg b.jpg .png
chext-strict: the word {@+:.}{-1}{:N} needs a path that doesn't exist yet, but 'b.png' already exists
```

Nothing is renamed. The checks can't catch two inputs that map to the same new name (`a.jpg a.gif .png`), because neither `a.png` exists yet when they run.

### A one-off helper with `rea eval`

For a per-item step that isn't worth its own alias, run a template through `rea eval` from `xargs`. Single-quote the inner template. `'…'` is fully literal, so the outer alias passes its braces through untouched:

```ini
backup-each = printf "%s\0" {@+:F} | xargs -0 -n1 rea eval 'cp -- {1} {1}.bak{:N}'
```

(If the inner template needs single quotes itself, use double quotes and escape its braces instead: `"cp -- \{1\} …"`.)

Errors from the inner template are labelled `rea` rather than with a name, which is the main reason to prefer a named `_helper` alias.

## Errors and exit status

rea reports problems as early as it can.

**When you create an alias** (`new`, `patch`, `check`), any template error is reported with a caret under the problem and a suggested fix. Nothing is written.

**When you run an alias**, the argument count, path checks and environment variables are all checked **before any command starts**. Failures print the alias's definition so you can see what it expected:

```
$ tojpg nope.png
tojpg: {1:F} needs an existing file, but 'nope.png' does not exist
    tojpg = convert {1:F} {1:.}.jpg{:N}
```

Exit statuses follow shell conventions:

| Status   | Meaning                                                                 |
|----------|-------------------------------------------------------------------------|
| `0`      | success                                                                 |
| `2`      | the alias refused to run: wrong argument count, failed check, or unset/empty `{$VAR}` |
| `126`    | a command was found but couldn't be executed                            |
| `127`    | a command wasn't found                                                  |
| `128+N`  | a command was killed by signal N (143 for SIGTERM, 130 for SIGINT)      |
| other    | the exit status of the command that ran, per the rules below            |

**Pipelines fail if any stage fails**, like bash's `set -o pipefail`. The status is the rightmost non-zero stage's, so `false | cat` fails. One exception: an earlier stage that dies of SIGPIPE because a later one stopped reading doesn't count as a failure. So `yes | head -n 2` succeeds, which it doesn't under bash's `pipefail`.

**Command lists** (`&&`, `||`, `;`) return the status of the last command that ran, as in sh.

rea's management commands (`new`, `patch`, `check`, `save`, …) exit 1 on errors and 2 on command-line usage errors. `eval` and `explain` exit with the status the alias itself would have.

## How aliases run

An alias is a copy of `rea-stub` with its definition written into it. Running it is self-contained: it doesn't need `rea`, and it doesn't read any configuration.

- **A single command** (no `|`, `&&`, `||` or `;`) replaces the alias process via `exec`. There's nothing left in between, so signals, the exit status and job control behave exactly as if you'd run the command directly.
- **Anything else** runs under a small supervisor process that starts the commands, wires up the pipes, and collects their statuses. Ctrl-C at a terminal goes to the whole process group and stops everything. The supervisor dies of the same signal, so your shell sees status 130.

`{0}` is the name the alias was **created** with, which is stored inside it. Renaming the file or symlinking to it doesn't change `{0}` or the name shown in error messages. To rename an alias, recreate it under the new name.

`rea new` won't overwrite a file that isn't a rea alias, such as a real program that happens to share the name, unless you pass `--replace-non-aliases`. Existing aliases are always updated without asking.

## Command reference

| Command | What it does |
|---------|--------------|
| `rea new [-o DIR] NAME TEMPLATE` | Create or update one alias |
| `rea patch [-o DIR] [--dry-run] [--prune] FILE [NAMES…]` | Create or update the aliases in an alias file |
| `rea save [--dry-run] FILE [DIR]` | Update an alias file from the aliases in a directory |
| `rea check FILE` | Validate an alias file |
| `rea list [DIR]` | Print a directory's aliases as an alias file |
| `rea inspect ALIAS…` | Print aliases' stored definitions |
| `rea explain [-t] ALIAS [ARGS…]` | Print what an alias would run, without running it |
| `rea eval TEMPLATE [ARGS…]` | Run a template once without creating an alias |
| `rea completions SHELL` | Print a completion script |
| `rea help [COMMAND]` | Help. `rea help` and `rea help new` include the full template syntax. |

`new` and `patch` also accept `--stub PATH` (the stub to copy, default `rea-stub` next to `rea`) and `--replace-non-aliases`.

## Gotchas

- **`#` is not a comment inside a template.** In an alias file, `gco = git switch {@+}  # change branch` passes `#`, `change` and `branch` to git. Comments must be on their own line.
- **`cd` and other builtins don't work.** See [above](#what-is-deliberately-not-supported).
- **Checks happen before anything runs.** `mkdir {1} && cp x {1:D}` fails if the directory doesn't exist yet, even though the first command would create it.
- **`patch` isn't all-or-nothing about name collisions.** An invalid *file* writes nothing. But if one alias would replace a non-alias program, that alias fails and the others are still written. The exit status is 1. `--dry-run` shows this in advance.
- **Killing an alias's PID alone** (for example `kill PID`, or `timeout` wrapping a pipeline alias) stops the supervisor but not the commands under it, the same as with `sh -c 'a | b'`. Signal the process group (`kill -- -PGID`), as a terminal's Ctrl-C does, to stop everything.