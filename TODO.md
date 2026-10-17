# TODO

Ideas that are designed or agreed on but not built yet. Newest first.

## Creation-time flags

Per-alias options chosen when the alias is created (`aliasmux new NAME
TEMPLATE --flag`, or in the alias file) and stored in the alias itself, so
they travel with it like the template does.

First use: `--runtime-file-check`. By default, file checks (`{1:F}` etc.,
below) all run up front, before anything starts. With this flag, each check
runs just before the pipeline that uses it is started instead, so a file an
earlier step creates is visible to a later check: `touch {1} && cat {1:F}`.
"Per pipeline" rather than per stage, because the stages of one pipeline all
start together.

There was at least one other use in mind for creation-time flags; it has
been forgotten for now, so add it here when it comes back.

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

## Path modifiers and file checks

Agreed design, not built. Both are written after a colon in a reference, and
work on every kind of reference: `{1}`, `{-1}`, `{@}`, slices (applied per
item).

Modifiers, purely textual (no file-system access), matching fd's behavior:

| Modifier | Gives | `dir/photo.jpg` |
|---|---|---|
| `{1:/}` | basename | `photo.jpg` |
| `{1://}` | parent directory | `dir` |
| `{1:.}` | path without extension | `dir/photo` |
| `{1:/.}` | basename without extension | `photo` |

Edge cases as in fd: the parent of a bare `file` is `.`; `a.tar.gz` loses only
`.gz`; a dotfile like `.bashrc` has no extension.

File checks, capital letters, checked against the argument itself before any
modifier (`{1:F/.}` checks the full path, then gives the stem):

| Check | Requires |
|---|---|
| `F` | an existing regular file (`test -f`) |
| `D` | an existing directory (`test -d`) |
| `E` | exists, either kind (`test -e`) |
| `N` | does not exist yet (`test ! -e`), e.g. `cp {1:F} {2:N}` |

- Checks run during expansion, before anything starts (see
  `--runtime-file-check` above for the alternative), and a failure exits 2
  like a wrong argument count: `flif270: {1} must be a file, but 'photos' is a
  directory`.
- Symlinks are followed, as `test -f` does.
- A check is a guard against mistakes, not a security boundary: a file can
  change between the check and the command.
- Arity is unaffected: modifiers and checks change how an argument is used,
  not which arguments are used.
