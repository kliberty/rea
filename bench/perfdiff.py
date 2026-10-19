#!/usr/bin/env python3
"""Subtract a bare program's cost from wrapped runs, from `perf stat -j` output.

Reads ``tz-perf-NAME.perf.json`` files, each written by something like::

    perf stat -j -o tz-perf-NAME.perf.json -r 1000 \\
        -e instructions:u,instructions:k ./program

and prints, for every NAME except the bare one, the user and kernel
instructions it costs on top of the bare run (``tz-perf-bare.perf.json`` by
default): what the wrapper itself adds.

    ./perfdiff.py                    # every tz-perf-*.perf.json here
    ./perfdiff.py --markdown         # as a Markdown table, e.g. for a PR
    ./perfdiff.py a.perf.json b.perf.json --bare-file base.perf.json
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path

# File names are PREFIX + NAME + SUFFIX; NAME labels the row.
PREFIX = "tz-perf-"
SUFFIX = ".perf.json"


@dataclass
class Counts:
    """Mean instruction counts from one `perf stat -r N` run.

    :ivar user: Mean user-space instructions per run.
    :ivar kernel: Mean kernel instructions per run.
    :ivar user_noise: perf's uncertainty of the user mean, in instructions.
    :ivar kernel_noise: Likewise for the kernel mean.
    """

    user: float = 0.0
    kernel: float = 0.0
    user_noise: float = 0.0
    kernel_noise: float = 0.0


def read_counts(path: Path) -> Counts:
    """Read the user and kernel instruction counts from a perf JSON file.

    perf writes one JSON object per line. Only lines whose event ends in
    ``:u`` or ``:k`` are used; anything else (other events, comments) is
    skipped. On hybrid CPUs perf reports each core type separately (e.g.
    ``cpu_core/instructions:u/`` and ``cpu_atom/instructions:u/``); a process
    runs on either, so those are added together.

    :param path: The file to read.
    :returns: The counts.
    :raises ValueError: If the file has no usable instruction counts.
    """
    counts = Counts()
    found: set[str] = set()
    for line in path.read_text().splitlines():
        line = line.strip()
        # Blank lines, and anything that isn't a JSON object (perf may add a
        # comment line), are not counter lines.
        if not line.startswith("{"):
            continue
        record = json.loads(line)
        # "cpu_core/instructions:u/" and "instructions:u" both end in ":u"
        # once any trailing "/" is removed.
        event = str(record.get("event", "")).rstrip("/")
        kind = event[-2:]
        if kind not in (":u", ":k"):
            continue
        # "<not counted>" or "<not supported>" instead of a number: the
        # counter didn't run, which would silently read as zero.
        try:
            value = float(record["counter-value"])
        except (KeyError, ValueError):
            print(f"{path}: {event} has no value ({record.get('counter-value')!r}); skipped",
                  file=sys.stderr)
            continue
        # perf's "variance" is the uncertainty of the mean, as a percentage.
        noise = value * float(record.get("variance", 0.0)) / 100
        if kind == ":u":
            counts.user += value
            counts.user_noise += noise
        else:
            counts.kernel += value
            counts.kernel_noise += noise
        found.add(kind)
    if found != {":u", ":k"}:
        raise ValueError(f"{path}: expected instructions:u and instructions:k, found {sorted(found) or 'neither'}")
    return counts


def label(path: Path) -> str:
    """The row label for a file: NAME from ``tz-perf-NAME.perf.json``.

    :param path: A perf JSON file.
    :returns: NAME, or the file name itself if it doesn't follow the pattern.
    """
    name = path.name
    if name.startswith(PREFIX) and name.endswith(SUFFIX):
        return name[len(PREFIX):-len(SUFFIX)]
    return name


def fmt(value: float, noise: float, show_noise: bool) -> str:
    """Format a count with thousands separators, and its noise if wanted.

    :param value: The count.
    :param noise: Its uncertainty.
    :param show_noise: Whether to append ``±noise``.
    :returns: E.g. ``"233,180"`` or ``"233,180 ±560"``.
    """
    text = f"{value:,.0f}"
    return f"{text} ±{noise:,.0f}" if show_noise else text


def main(argv: list[str] | None = None) -> int:
    """Parse arguments, subtract the bare counts, and print the table.

    :param argv: Arguments without the program name; None for sys.argv.
    :returns: A process exit status.
    """
    parser = argparse.ArgumentParser(
        description="Instructions each wrapped run adds over the bare one, from perf stat -j output.")
    parser.add_argument("files", nargs="*", type=Path,
                        help=f"perf JSON files (default: every {PREFIX}*{SUFFIX} in the current directory)")
    parser.add_argument("--bare", default="bare",
                        help="NAME of the unwrapped run, as in %(prog)s's file pattern (default: %(default)s)")
    parser.add_argument("--bare-file", type=Path,
                        help="the unwrapped run's file, if it doesn't follow the pattern")
    parser.add_argument("--markdown", action="store_true", help="print a Markdown table")
    parser.add_argument("--noise", action="store_true",
                        help="show perf's uncertainty of each mean (the bare run's is added in)")
    args = parser.parse_args(argv)

    files = args.files or sorted(Path(".").glob(f"{PREFIX}*{SUFFIX}"))
    bare_file = args.bare_file or Path(f"{PREFIX}{args.bare}{SUFFIX}")
    if not bare_file.exists():
        parser.error(f"no bare run: {bare_file} not found")
    # The bare file may be among the inputs (it is, with the default glob);
    # it is the baseline, not a row.
    files = [f for f in files if f.resolve() != bare_file.resolve()]
    if not files:
        parser.error("no perf files to compare")

    try:
        bare = read_counts(bare_file)
        rows = [(label(f), read_counts(f)) for f in files]
    except (OSError, ValueError) as err:
        print(f"perfdiff: {err}", file=sys.stderr)
        return 1

    header = ["", "user", "kernel"]
    table = [header]
    for name, c in rows:
        # Uncertainties of a difference add (a conservative bound, not the
        # root-sum-square), which keeps the arithmetic obvious.
        table.append([
            name,
            fmt(c.user - bare.user, c.user_noise + bare.user_noise, args.noise),
            fmt(c.kernel - bare.kernel, c.kernel_noise + bare.kernel_noise, args.noise),
        ])

    if args.markdown:
        print("| " + " | ".join(header) + " |")
        print("|---|---:|---:|")
        for row in table[1:]:
            print("| " + " | ".join(row) + " |")
    else:
        # Names left-aligned, numbers right-aligned, each column as wide as
        # its widest cell.
        widths = [max(len(row[i]) for row in table) for i in range(len(header))]
        for row in table:
            print("  ".join([row[0].ljust(widths[0]), *(cell.rjust(w) for cell, w in zip(row[1:], widths[1:]))]))
    # The baseline, so the table can be read on its own.
    print(f"\n(minus {label(bare_file)}: {bare.user:,.0f} user, {bare.kernel:,.0f} kernel)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
