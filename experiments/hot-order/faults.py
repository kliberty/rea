#!/usr/bin/env python3
"""Count the minor page faults of a command, without perf.

Runs the command repeatedly and reports the minor faults ``wait4`` gives for
it. Those include any children it waited for (a supervisor's pipeline), so
compare a command against itself under different builds, not across commands.

    ./faults.py -n 200 -- ./t
"""

from __future__ import annotations

import argparse
import os
import statistics
import subprocess
import sys


def faults(cmd: list[str]) -> int:
    """Run cmd once, output discarded, and return its minor fault count.

    :param cmd: The command and its arguments.
    :returns: ru_minflt from wait4.
    """
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL)
    # wait4 rather than proc.wait(), which discards the rusage.
    _, _, usage = os.wait4(proc.pid, 0)
    # Tell Popen the child is gone, so it doesn't try to reap it again.
    proc.returncode = 0
    return usage.ru_minflt


def main(argv: list[str] | None = None) -> int:
    """Parse arguments, run the command, print the fault statistics.

    :param argv: Arguments without the program name; None for sys.argv.
    :returns: A process exit status.
    """
    parser = argparse.ArgumentParser(description="Minor page faults of a command over repeated runs.")
    parser.add_argument("-n", type=int, default=100, help="runs (default: %(default)s)")
    parser.add_argument("cmd", nargs="+", help="the command to run")
    args = parser.parse_args(argv)
    counts = [faults(args.cmd) for _ in range(args.n)]
    # Median and min: the stack's random offset adds a fault on some runs,
    # so the minimum is the layout's own count and the median the typical one.
    print(f"median {statistics.median(counts):g}  min {min(counts)}  max {max(counts)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
