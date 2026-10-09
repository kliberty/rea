#!/usr/bin/env python3
"""Build every aliasmux packaging variant side by side, for comparison.

Output layout (under ``--out``, default ``out/``)::

    multicall/  aliasmux + one symlink per alias
    stub/       one patched Rust stub per alias
    cdylib/     one patched C stub per alias, plus lib/libaliasmux_shared.so
    single/     one separately compiled executable per alias

All four run the same runtime (``crates/core``) on the same alias file.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

# The workspace root is the parent of this script's directory.
ROOT = Path(__file__).resolve().parent.parent


def run(command: list[str], env: dict[str, str] | None = None, capture: bool = False) -> str:
    """Run a command from the workspace root, stopping the script if it fails.

    :param command: argv to run
    :param env: extra environment variables, on top of the current ones
    :param capture: return stdout instead of letting it through
    :return: captured stdout, or an empty string
    """
    # Show each step, so a failing cargo invocation can be rerun by hand.
    prefix = " ".join(f"{k}={v}" for k, v in (env or {}).items())
    print(f"+ {prefix} {' '.join(command)}".replace("+  ", "+ "), file=sys.stderr)
    # Merge rather than replace the environment: cargo needs PATH, HOME, etc.
    result = subprocess.run(command, cwd=ROOT, env={**os.environ, **(env or {})}, capture_output=capture, text=True)
    if result.returncode != 0:
        # Captured output would otherwise be lost on failure.
        if capture:
            sys.stderr.write(result.stderr)
        sys.exit(f"command failed with status {result.returncode}")
    return result.stdout if capture else ""


def needed_libraries(binary: Path) -> list[str]:
    """List a binary's dynamic library dependencies (ELF DT_NEEDED entries).

    :param binary: an ELF executable or shared library
    :return: library names, empty for a static binary
    """
    # readelf (binutils) reads the dynamic section without loading anything,
    # unlike ldd, which runs the binary's loader.
    dynamic = run(["readelf", "-d", str(binary)], capture=True)
    return re.findall(r"\(NEEDED\)\s+Shared library: \[([^\]]+)\]", dynamic)


def alias_names(aliasmux: Path) -> list[str]:
    """Ask the built multicall binary for the alias names.

    Reusing ``aliasmux list`` keeps a single parser for the alias file.

    :param aliasmux: path to the multicall binary
    :return: alias names, in file order
    """
    # Each line is "name  template"; names never contain whitespace.
    return [line.split(maxsplit=1)[0] for line in run([str(aliasmux), "list"], capture=True).splitlines() if line]


def fresh_dir(path: Path) -> Path:
    """Create an empty directory, removing any previous contents.

    :param path: directory to (re)create
    :return: the same path
    """
    # Stale aliases from an earlier run would skew the size comparison.
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True)
    return path


def libc_rustflags(target: str, static_glibc: bool) -> str:
    """RUSTFLAGS selecting how the C library is linked.

    :param target: Rust target triple
    :param static_glibc: link glibc statically (needs glibc's static libraries,
        e.g. Debian's libc6-dev or Fedora's glibc-static)
    :return: flags string (possibly empty)
    """
    # musl targets are static by default, so need no flags. glibc targets are
    # dynamic by default; crt-static makes them static, which works here
    # because the runtime never uses NSS or dlopen.
    return "-C target-feature=+crt-static" if static_glibc and target.endswith("-gnu") else ""


def build_multicall_and_stub(target: str, rustflags: str, out: Path) -> tuple[Path, Path]:
    """Build the multicall binary and the Rust stub, and set up multicall/.

    :param target: Rust target triple
    :param rustflags: RUSTFLAGS from :func:`libc_rustflags`
    :param out: output root
    :return: paths of the built aliasmux and Rust stub
    """
    # One cargo invocation for both, sharing the compiled runtime.
    run(["cargo", "build", "--release", "--target", target, "-p", "aliasmux", "-p", "aliasmux-stub"],
        env={"RUSTFLAGS": rustflags})
    built = ROOT / "target" / target / "release"
    # The multicall variant: the binary plus a symlink per alias.
    multicall = fresh_dir(out / "multicall")
    shutil.copy2(built / "aliasmux", multicall / "aliasmux")
    run([str(multicall / "aliasmux"), "install", str(multicall)], capture=True)
    return built / "aliasmux", built / "aliasmux-stub"


def build_cdylib_stub(out: Path) -> Path:
    """Build the runtime as a C-ABI shared library plus the C stub that uses it.

    The library is staged in ``cdylib-lib-staging/``, which ``main`` moves to
    ``cdylib/lib/`` once the stubs are patched.

    Constraint on change: this always uses the host's gnu target with no
    crt-static flag, because a cdylib can't be linked with a static C runtime.

    :param out: output root
    :return: path of the built C stub
    """
    host = run(["rustc", "--print", "host-tuple"], capture=True).strip()
    # A separate target dir, because the static builds use different
    # RUSTFLAGS, and sharing one dir would invalidate the cache every run.
    target_dir = ROOT / "target" / "cdylib"
    # The normal release profile works: a cdylib carries its own std, so LTO
    # and panic=abort are fine (unlike a Rust-ABI dylib).
    run(["cargo", "build", "--release", "--target", host, "-p", "aliasmux-shared"],
        env={"CARGO_TARGET_DIR": str(target_dir)})
    built = target_dir / host / "release"

    # Compile the C stub against the library. The rpath makes each alias look
    # for the library in lib/ next to itself, wherever the pair is copied.
    c_dir = ROOT / "crates" / "shared" / "c"
    stub = built / "aliasmux-cstub"
    run(["cc", "-O2", "-Wall", "-Wextra", "-o", str(stub), str(c_dir / "stub.c"), f"-I{c_dir}",
         f"-L{built}", "-laliasmux_shared", "-Wl,-rpath,$ORIGIN/lib"])

    # Only our own library needs shipping; libc and friends stay with the system.
    lib = fresh_dir(out / "cdylib-lib-staging")
    shutil.copy2(built / "libaliasmux_shared.so", lib / "libaliasmux_shared.so")
    return stub


def patch_variant(aliasmux: Path, stub: Path, directory: Path) -> None:
    """Fill a directory with one patched copy of a stub per alias.

    :param aliasmux: the management binary, which does the patching
    :param stub: stub to copy
    :param directory: output directory (emptied first)
    """
    run([str(aliasmux), "patch", "--stub", str(stub), "--out", str(fresh_dir(directory))], capture=True)


def build_singles(target: str, rustflags: str, names: list[str], out: Path) -> None:
    """Compile one executable per alias.

    :param target: Rust target triple
    :param rustflags: RUSTFLAGS from :func:`libc_rustflags`
    :param names: aliases to build
    :param out: output root
    """
    single = fresh_dir(out / "single")
    built = ROOT / "target" / target / "release" / "aliasmux-single"
    # Same RUSTFLAGS and target dir as the multicall build, so the runtime is
    # compiled once and each alias only recompiles its tiny main and relinks.
    for name in names:
        run(["cargo", "build", "--release", "--target", target, "-p", "aliasmux-single"],
            env={"RUSTFLAGS": rustflags, "ALIASMUX_NAME": name})
        shutil.copy2(built, single / name)


def describe(path: Path) -> str:
    """One-line linkage summary for a binary.

    :param path: the binary
    :return: "static", or the list of shared libraries it needs
    """
    libraries = needed_libraries(path)
    return "static" if not libraries else ", ".join(libraries)


def report(out: Path, names: list[str]) -> None:
    """Print size and linkage for each variant.

    :param out: output root
    :param names: alias names
    """
    # Per variant: the files that make it up, and which one represents an alias.
    variants = {
        "multicall": ([out / "multicall" / "aliasmux"], out / "multicall" / "aliasmux"),
        "stub": ([out / "stub" / n for n in names], out / "stub" / names[0]),
        "cdylib": ([out / "cdylib" / n for n in names] + sorted((out / "cdylib" / "lib").iterdir()), out / "cdylib" / names[0]),
        "single": ([out / "single" / n for n in names], out / "single" / names[0]),
    }
    print(f"\n{len(names)} aliases\n")
    print(f"{'variant':10} {'per alias':>10} {'total':>10}  linkage")
    for variant, (files, sample) in variants.items():
        total = sum(f.stat().st_size for f in files)
        # Symlinks cost nothing, so a multicall alias is "free" past the binary.
        per_alias = 0 if variant == "multicall" else sample.stat().st_size
        print(f"{variant:10} {per_alias / 1024:9.0f}K {total / 1024:9.0f}K  {describe(sample)}")


def main() -> None:
    """Parse arguments and build every variant."""
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    # Arbitrary default output location; anything works.
    parser.add_argument("--out", type=Path, default=ROOT / "out", help="output directory (default: out/)")
    parser.add_argument("--target", default="x86_64-unknown-linux-musl",
                        help="target for the multicall, stub and single variants (default: "
                             "x86_64-unknown-linux-musl, which is always static; a -gnu target "
                             "links glibc dynamically unless --static-glibc is given)")
    parser.add_argument("--static-glibc", action="store_true",
                        help="with a -gnu target, link glibc statically (needs glibc's static libraries)")
    parser.add_argument("--aliases", type=Path, help="alias file to use instead of aliases.txt")
    args = parser.parse_args()

    # The build scripts read the alias file location from the environment.
    # Absolute, because cargo resolves it relative to the workspace root.
    if args.aliases:
        os.environ["ALIASMUX_ALIASES"] = str(args.aliases.resolve())
    out = args.out.resolve()

    # Order matters: the multicall binary does the patching and lists names.
    rustflags = libc_rustflags(args.target, args.static_glibc)
    aliasmux, rust_stub = build_multicall_and_stub(args.target, rustflags, out)
    names = alias_names(aliasmux)
    patch_variant(aliasmux, rust_stub, out / "stub")

    c_stub = build_cdylib_stub(out)
    patch_variant(aliasmux, c_stub, out / "cdylib")
    # The library goes in cdylib/lib, matching the stub's $ORIGIN/lib rpath.
    (out / "cdylib-lib-staging").rename(out / "cdylib" / "lib")

    build_singles(args.target, rustflags, names, out)
    report(out, names)


if __name__ == "__main__":
    main()
