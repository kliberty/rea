# TODO

Ideas that are designed or agreed on but not built yet. Newest first.

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
