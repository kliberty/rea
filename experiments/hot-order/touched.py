#!/usr/bin/env python3
"""Estimate an alias's page faults from a lackey trace, split by kind.

A model, not a measurement: one fault per 64 KB fault-around window of the
binary that's executed or read, and one per 4 KB page written. It ignores
pages the kernel maps during exec and the clamping of windows to mapping
edges, so absolute numbers are off; it's for seeing where faults go and
what a layout change moves. Assumes a non-PIE binary (addresses as linked).

    ./touched.py ./t
    ./touched.py ./fmt f.txt a b d
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
from collections import defaultdict
from pathlib import Path


def segments(binary: str) -> list[tuple[int, int]]:
    """The binary's PT_LOAD address ranges.

    :param binary: The ELF file.
    :returns: (start, end) of each loadable segment in memory.
    """
    out = subprocess.run(["readelf", "-lW", binary], check=True, capture_output=True, text=True).stdout
    ranges = []
    for line in out.splitlines():
        p = line.split()
        # LOAD lines: Type, Offset, VirtAddr, PhysAddr, FileSiz, MemSiz, ...
        if p and p[0] == "LOAD":
            ranges.append((int(p[2], 16), int(p[2], 16) + int(p[5], 16)))
    return ranges


def main(argv: list[str] | None = None) -> int:
    """Trace the command and print the estimated faults by kind.

    :param argv: Arguments without the program name; None for sys.argv.
    :returns: A process exit status.
    """
    parser = argparse.ArgumentParser(description="Estimated page faults of an alias, by kind.")
    parser.add_argument("cmd", nargs=argparse.REMAINDER, help="the alias (a non-PIE binary) and its arguments")
    args = parser.parse_args(argv)
    if not args.cmd:
        parser.error("no command given")
    segs = segments(args.cmd[0])

    with tempfile.TemporaryDirectory() as d:
        # Only the alias's own process: forked children would log to the same
        # file and blur the count.
        subprocess.run(["valgrind", "--tool=lackey", "--trace-mem=yes", f"--log-file={d}/lk", *args.cmd],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        text = Path(d, "lk").read_text()

    # Kind of access -> the set of windows or pages it touched.
    kinds: dict[str, set[int]] = defaultdict(set)
    for line in text.splitlines():
        # "I" instruction, " L" load, " S" store, " M" modify (load+store).
        op = line[:2].strip()
        if op not in ("I", "L", "S", "M"):
            continue
        addr = int(line[3:line.index(",")], 16)
        in_binary = any(a <= addr < b for a, b in segs)
        if op in ("S", "M"):
            # Writes fault page by page: copy-on-write or fresh zero pages,
            # neither of which gets fault-around.
            kinds["writes to the binary" if in_binary else "writes elsewhere (stack, heap)"].add(addr >> 12)
        elif op == "I":
            kinds["code windows"].add(addr >> 16)
        elif in_binary:
            kinds["data read windows"].add(addr >> 16)
    for kind, pages in sorted(kinds.items()):
        print(f"  {kind:32} {len(pages)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
