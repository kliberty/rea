#!/usr/bin/env python3
"""Make a linker symbol ordering file from the functions an alias runs.

Each workload (an alias and its arguments) runs under valgrind's lackey
tool, which logs every instruction executed. Those addresses are mapped to
the stub's function symbols, in the order the functions first ran, and the
workloads' lists are merged in the order given: the first workload's
functions first, then whatever each later one adds. So put the commonest
case first; its code ends up at the front, where it can share as few pages
as possible.

The result is for lld's ``--symbol-ordering-file``:

    ./hot_symbols.py --stub path/to/aliasmux-stub -o hot.txt \\
        --run "./t" --run "./fmt f.txt a b d" --run "./sup c a b"

The aliases must be copies of the same stub binary: only it is symbolized.
"""

from __future__ import annotations

import argparse
import bisect
import shlex
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Function:
    """A function symbol in the stub.

    :ivar start: Its address.
    :ivar size: Its size in bytes.
    :ivar names: Every symbol at this address (identical-code folding and
        aliases can give one function several names).
    """

    start: int
    size: int
    names: list[str]


def read_functions(stub: Path) -> list[Function]:
    """List the stub's function symbols, sorted by address.

    :param stub: The stub binary, with its symbol table (not stripped).
    :returns: One Function per address that has a sized code symbol.
    """
    # -S: sizes; -n: sorted by address; no -C, since the ordering file
    # needs the mangled names the linker sees.
    out = subprocess.run(["nm", "-nS", "--defined-only", str(stub)],
                         check=True, capture_output=True, text=True).stdout
    functions: list[Function] = []
    for line in out.splitlines():
        parts = line.split()
        # Sized symbols have four fields: address, size, type, name.
        if len(parts) != 4 or parts[2] not in "tTwWiI":
            continue
        start, size, name = int(parts[0], 16), int(parts[1], 16), parts[3]
        if functions and functions[-1].start == start:
            functions[-1].names.append(name)
        else:
            functions.append(Function(start, size, [name]))
    # A stripped binary has no symbols to order by; nm prints nothing (or
    # "no symbols" on stderr) rather than failing.
    if not functions:
        raise ValueError(f"{stub}: no function symbols (stripped? build with CARGO_PROFILE_RELEASE_STRIP=false)")
    return functions


def build_id(binary: Path) -> str:
    """The binary's GNU build ID, which patching an alias leaves unchanged.

    :param binary: An ELF file.
    :returns: The ID in hex.
    :raises ValueError: If the binary has no build ID note.
    """
    out = subprocess.run(["readelf", "-n", str(binary)], check=True, capture_output=True, text=True).stdout
    for line in out.splitlines():
        if "Build ID:" in line:
            return line.split("Build ID:")[1].strip()
    raise ValueError(f"{binary}: no build ID to match the stub with")


def entry_point(binary: Path) -> tuple[int, bool]:
    """The binary's entry address as linked, and whether it's a PIE.

    :param binary: An ELF file.
    :returns: (entry address, True if position-independent).
    """
    out = subprocess.run(["readelf", "-h", str(binary)], check=True, capture_output=True, text=True).stdout
    fields = dict(line.split(":", 1) for line in out.splitlines() if ":" in line)
    return int(fields["  Entry point address"], 16), fields["  Type"].strip().startswith("DYN")


def load_bias(addresses: list[int], entry: int, pie: bool) -> int:
    """How far from its linked addresses the traced stub was loaded.

    A PIE loads wherever the loader (here valgrind) puts it, so traced
    addresses are the linked ones plus an offset. Assumption: the stub is
    static, with no dynamic loader, so the first instruction traced is the
    entry point, which gives the offset.

    :param addresses: The trace, parent first.
    :param entry: The entry address as linked.
    :param pie: Whether the stub is position-independent.
    :returns: The offset to subtract; 0 for a non-PIE.
    :raises ValueError: If the trace doesn't start at a plausible entry.
    """
    if not pie:
        return 0
    bias = addresses[0] - entry
    # Binaries load at page boundaries; anything else means the trace didn't
    # start at the entry point (a dynamically linked stub, say).
    if bias % 0x1000:
        raise ValueError(f"trace starts at {addresses[0]:#x}, not at the entry point "
                         f"{entry:#x} plus a page-aligned offset; is the stub statically linked?")
    return bias


def trace(cmd: list[str], logdir: Path) -> list[int]:
    """Run cmd under lackey and return the instruction addresses it executed.

    Forked children (a supervisor's pipeline stages) get their own logs; they
    are read after the parent's, in pid order. Programs the stub execs are
    not traced: lackey stops at exec without --trace-children.

    :param cmd: The alias and its arguments.
    :param logdir: An empty directory for lackey's logs.
    :returns: Addresses in execution order, parent first.
    """
    # The alias's own exit status doesn't matter (a failed check still runs
    # the code worth ordering), so it isn't checked; valgrind's own errors
    # end up in its log, which is shown if nothing was traced.
    try:
        subprocess.run(["valgrind", "--tool=lackey", "--trace-mem=yes",
                        f"--log-file={logdir}/lk.%p", *cmd],
                       check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except FileNotFoundError:
        raise ValueError("valgrind not found") from None
    addresses: list[int] = []
    # Sorted by pid as a number; the parent is the lowest.
    for log in sorted(logdir.glob("lk.*"), key=lambda p: int(p.suffix[1:])):
        with log.open() as f:
            for line in f:
                # Instruction lines are "I  addr,size"; loads and stores
                # start with a space, and valgrind's own lines with "==".
                if line.startswith("I"):
                    addresses.append(int(line[3:line.index(",")], 16))
    if not addresses:
        logs = "".join(log.read_text() for log in logdir.glob("lk.*"))
        raise ValueError(f"{shlex.join(cmd)}: lackey traced no instructions; its log:\n{logs or '(none)'}")
    return addresses


def first_use(addresses: list[int], functions: list[Function], bias: int) -> list[Function]:
    """The functions the addresses fall in, in order of first execution.

    :param addresses: Instruction addresses in execution order.
    :param functions: The stub's functions, sorted by address.
    :param bias: The load offset to subtract from each address.
    :returns: Each function hit, once, at the position of its first hit.
    """
    starts = [f.start for f in functions]
    seen: set[int] = set()
    order: list[Function] = []
    for traced in addresses:
        addr = traced - bias
        i = bisect.bisect_right(starts, addr) - 1
        # Outside the stub (the vdso, valgrind trampolines) or between
        # symbols: nothing to order.
        if i < 0 or addr >= functions[i].start + functions[i].size:
            continue
        if i not in seen:
            seen.add(i)
            order.append(functions[i])
    return order


def main(argv: list[str] | None = None) -> int:
    """Trace each workload, merge the orders, and write the ordering file.

    :param argv: Arguments without the program name; None for sys.argv.
    :returns: A process exit status.
    """
    parser = argparse.ArgumentParser(description="Symbol ordering file from traced alias runs.")
    parser.add_argument("--stub", type=Path, required=True, help="the stub the aliases were made from")
    parser.add_argument("--run", action="append", required=True, metavar="CMD",
                        help="an alias and its arguments, shell-quoted; repeat, commonest first")
    parser.add_argument("-o", "--output", type=Path, required=True, help="the ordering file to write")
    args = parser.parse_args(argv)

    merged: list[Function] = []
    have: set[int] = set()
    try:
        functions = read_functions(args.stub)
        orders = []
        stub_id = build_id(args.stub)
        entry, pie = entry_point(args.stub)
        for run in args.run:
            cmd = shlex.split(run)
            # An alias from another build has the same symbol names at other
            # addresses, which would map to the wrong functions without any
            # sign of it, so the builds must match.
            alias = shutil.which(cmd[0])
            if alias is None:
                raise ValueError(f"{cmd[0]}: not found")
            if build_id(Path(alias)) != stub_id:
                raise ValueError(f"{cmd[0]}: not made from {args.stub} (build IDs differ)")
            with tempfile.TemporaryDirectory() as logdir:
                addresses = trace(cmd, Path(logdir))
            order = first_use(addresses, functions, load_bias(addresses, entry, pie))
            # Instructions ran but none in the stub's functions: the address
            # mapping is off somehow, and an empty file would hide that.
            if not order:
                raise ValueError(f"{run}: none of its {len(addresses):,} traced instructions "
                                 f"map to {args.stub}'s functions")
            orders.append((run, order))
    except (OSError, ValueError, subprocess.CalledProcessError) as err:
        print(f"hot_symbols: {err}", file=sys.stderr)
        return 1
    for run, order in orders:
        added = [f for f in order if f.start not in have]
        have.update(f.start for f in added)
        merged.extend(added)
        # How much each workload adds, to see whether the common path fits
        # in one fault-around window (64 KB by default).
        print(f"{run}: {len(order)} functions, {len(added)} new, "
              f"{sum(f.size for f in added):,} bytes new, {sum(f.size for f in merged):,} total",
              file=sys.stderr)
    args.output.write_text("".join(f"{name}\n" for f in merged for name in f.names))
    return 0


if __name__ == "__main__":
    sys.exit(main())
