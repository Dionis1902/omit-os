#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Checks two rules of IP_POLICY.md that a machine can check.

1. Every source file of the Windows compatibility code says, in a
   "Sources:" comment near its top, what it was written from.
2. Every file of imported Linux code is listed in PROVENANCE.md.

    python3 tools/check-ip.py
"""

import subprocess
import sys

# Where the Windows compatibility code lives.
WINDOWS = ("compat/windows/", "userspace/programs/src/bin/winrun/")
# Where imported Linux code lives; its README is ours.
LINUX_IMPORTS = "drivers/linux/"
OURS_AMONG_IMPORTS = ("drivers/linux/README.md",)
SOURCE_SUFFIXES = (".rs", ".c", ".h", ".S")
HEADER_LINES = 14


def main():
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    files = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True, cwd=root
    ).stdout.split("\n")
    with open(f"{root}/PROVENANCE.md", encoding="utf-8") as manifest:
        provenance = manifest.read()
    problems = []
    for path in filter(None, files):
        if path.startswith(WINDOWS) and path.endswith(SOURCE_SUFFIXES):
            with open(f"{root}/{path}", encoding="utf-8", errors="replace") as source:
                head = [next(source, "") for _ in range(HEADER_LINES)]
            if not any("Sources:" in line for line in head):
                problems.append(f"{path}: no 'Sources:' comment in its first {HEADER_LINES} lines")
        if path.startswith(LINUX_IMPORTS) and path not in OURS_AMONG_IMPORTS:
            if f"`{path}`" not in provenance:
                problems.append(f"{path}: imported file not listed in PROVENANCE.md")
        if path.startswith(LINUX_IMPORTS) and path.startswith(WINDOWS):
            problems.append(f"{path}: Linux imports and Windows code must not share a directory")
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print(f"{len(problems)} problem(s); see IP_POLICY.md", file=sys.stderr)
        return 1
    print("Windows-compatibility sources and Linux imports are accounted for")
    return 0


if __name__ == "__main__":
    sys.exit(main())
