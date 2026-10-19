# Experiment: gathering the stub's hot code into fewer pages

Status: tooling works; the gain measured so far is small. Kept for what it
shows about page faults and linking, and to pick up again later. See the
"Gather the stub's hot code" item in [TODO.md](../../TODO.md).

## Why page faults

On a static musl stub, after the `no_main` and lexer work, most of an alias
call's remaining cost is in the kernel, and the clearest difference from a
minimal C++ wrapper is page faults: 10 against 4 (`perf trace -F all
--no-syscalls ./ALIAS`).

When a process first reads a page of a file it has mapped, the kernel maps
the surrounding 64 KB too, as long as those pages are in the page cache
("fault-around"; the size is the kernel's `fault_around_bytes`, 64 KB by
default). The window is 64 KB-aligned and
stops at the edges of the mapping (VMA). Writes don't get fault-around: each
written page faults once (copy-on-write, or a fresh zero page).

So a binary whose startup code is spread across 500 KB faults once per 64 KB
window it touches, while one whose startup code sits together faults once.

## What one musl trace showed

Mapped to sections with `readelf -lSW`, the 10 faults of a short alias were:

| fault | what | avoidable? |
|---|---|---|
| kernel `rep_stos` | exec zeroing the `.data`/`.bss` page tail | no |
| kernel `rep_movs` | exec copying argv and environment to the stack | no |
| `_start` | first code page | no |
| `__libc_start_main` | code ~350 KB away | by ordering |
| `__init_libc` → anon | first write to `.bss` past the file | no (one is needed) |
| `static_init_tls` → `@0x40` | musl reading the program headers | by layout |
| `libc_start_init` → `.init_array` | first page of the RW segment | hard |
| `main` → `.rodata` | the template slot | hard (linker script) |
| `from_utf8`, `next_match` | code in two more windows | by ordering |

Two findings from the section table:

- GNU ld's default `-z separate-code` puts the program headers in a mapping
  of their own, so reading them always faults. `-z noseparate-code` merges
  headers, `.text` and `.rodata` into one mapping, which removed that fault
  (at the cost of `.rodata` and the headers being executable).
- An extra stack fault appears on some runs and not others: the kernel
  randomizes the stack's offset within its page, so whether a call chain
  crosses into the next page varies. Average over many runs.

## The tools

All three run under valgrind, so neither perf nor root is needed.

- `hot_symbols.py` traces aliases with valgrind's lackey (every instruction
  executed, in order), maps the addresses to the stub's functions, and writes
  them in order of first execution as an lld ordering file. Several
  workloads merge in the order given: put the commonest first.
- `faults.py` runs a command many times and reports its minor faults from
  `wait4`. They include waited-for children, so compare a command with
  itself across builds.
- `touched.py` is a rough model of where the faults go (code windows, data
  windows, written pages), from one lackey trace of a non-PIE binary.

`aliases.txt` has the three workloads used so far: a trivial alias, one with
checks, modifiers, a slice and an env var, and a supervised pipeline and
list. A layout that only covers the trivial case would leave the expansion
and supervisor code scattered.

## Steps

```sh
# 1. A stub with symbols, built the way the final one will be (PIE or not:
#    the generated code differs, so the names may not all match otherwise).
CARGO_PROFILE_RELEASE_STRIP=false make build-static PIE=no

# 2. Aliases from it, and the ordering file.
mkdir -p /tmp/hot && aliasmux patch experiments/hot-order/aliases.txt -o /tmp/hot --stub STUB
touch f.txt && mkdir -p d
experiments/hot-order/hot_symbols.py --stub STUB -o hot.txt \
    --run "/tmp/hot/t" --run "/tmp/hot/fmt f.txt a b d" --run "/tmp/hot/sup c a b"

# 3. Link with it. musl links through the system cc and GNU ld, which has no
#    --symbol-ordering-file; -B points cc at rustup's own lld instead.
GCC_LD="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/gcc-ld"
RUSTFLAGS="-C relocation-model=static -C link-arg=-Wl,-z,noseparate-code \
  -C link-arg=-B$GCC_LD -C link-arg=-fuse-ld=lld \
  -C link-arg=-Wl,--symbol-ordering-file=$PWD/hot.txt \
  -C link-arg=-Wl,--no-warn-symbol-ordering" make build-static

# 4. Compare.
experiments/hot-order/faults.py -n 300 -- ./ALIAS
```

## Results so far

Prototype on a static non-PIE **glibc** stub (the musl target wasn't
available where it was built), minor faults per call, median of 300 runs:

| alias | before | ordered |
|---|---:|---:|
| `t` | 102 | 98 |
| `fmt` | 117 | 112 |
| `sup` | 455 | 450 |

These include the exec'd programs' own faults (`/bin/true` alone is 61), so
only the differences mean anything. glibc is a poor test: its startup alone
is about 95 KB of code, so even ordered it needs two or three windows, and it
picks `memcpy` variants per CPU at startup (valgrind doesn't emulate
AVX-512), so the traced variants aren't the ones a native run uses. musl has
neither problem; on musl the measured difference was also small, and not yet
looked into in detail.

## Pitfalls met along the way

- **Stripped stubs.** The release profile has `strip = true`; the tracer
  needs symbols (`CARGO_PROFILE_RELEASE_STRIP=false`).
- **PIE stubs.** `nm` gives linked addresses; valgrind loads a PIE
  elsewhere. The tracer takes the offset from the entry point (the first
  instruction a static binary runs).
- **Mixed builds.** Function names match across builds but addresses don't,
  so an alias from another build would map silently to the wrong functions.
  The tracer compares build IDs, which patching an alias leaves alone.
- **Staleness.** Rust symbol names carry a hash that changes with the code
  and flags. `--no-warn-symbol-ordering` hides the entries that no longer
  match; leave it off to see them, and regenerate after code changes.
- **`RUSTFLAGS` without `--target`.** Cargo applies them to proc macros too;
  `relocation-model=static` then breaks the link of `clap_derive`. The static
  build passes `--target`, so it's unaffected.
