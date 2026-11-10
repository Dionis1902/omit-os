#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Checks that every source file carries the licence of its component.

The mapping of directories to licences is the one in COPYING. Imported
Linux driver code (drivers/linux/) keeps its upstream notices and is not
checked. Run from anywhere inside the repository:

    python3 tools/check-licenses.py
"""

import subprocess
import sys

GPL2 = "GPL-2.0-only"
SYSCALL = "GPL-2.0-only WITH LicenseRef-OmitOS-syscall-note"
GPL3 = "GPL-3.0-or-later"
PERMISSIVE = "MIT OR Apache-2.0"

# Longest prefix wins.
COMPONENTS = {
    "kernel/": GPL2,
    "boot/": GPL2,
    "shell/": GPL2,
    "compat/linux-kpi/": GPL2,
    "libs/acpi/": GPL2,
    "libs/bootproto/": GPL2,
    "libs/elf/": GPL2,
    "libs/ext2/": GPL2,
    "libs/fat/": GPL2,
    "libs/gpt/": GPL2,
    "libs/hwdb/": GPL2,
    "libs/net/": GPL2,
    "libs/term/": GPL2,
    "libs/vfs/": GPL2,
    "libs/abi/": SYSCALL,
    "userspace/programs/": GPL3,
    "compat/windows/pe/": GPL3,
    "compat/windows/win32/": GPL3,
    "userspace/rt/": PERMISSIVE,
    "libs/gfx/": PERMISSIVE,
    "libs/mm/": PERMISSIVE,
    "libs/window/": PERMISSIVE,
    "compat/windows/samples/": PERMISSIVE,
    "tools/": PERMISSIVE,
}
# Upstream code: notices are kept as they came.
IMPORTED = ("drivers/linux/",)
SOURCE_SUFFIXES = (".rs", ".c", ".h", ".S", ".ld", ".py", ".sh")
# Written in two pieces so that tools scanning for licence tags do not
# take these lines for one.
TAG = "SPDX-License-" + "Identifier:"
COPYRIGHT = "SPDX-FileCopyright" + "Text: 2026 Omit OS contributors"


def expected(path):
    matches = [prefix for prefix in COMPONENTS if path.startswith(prefix)]
    return COMPONENTS[max(matches, key=len)] if matches else None


def main():
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    files = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True, cwd=root
    ).stdout.split("\n")
    problems = []
    for path in filter(None, files):
        if not path.endswith(SOURCE_SUFFIXES) or path.startswith(IMPORTED):
            continue
        want = expected(path)
        if want is None:
            problems.append(f"{path}: not in any component; add it to COPYING and this script")
            continue
        with open(f"{root}/{path}", encoding="utf-8", errors="replace") as source:
            head = [next(source, "") for _ in range(6)]
        licences = [
            line.split(TAG, 1)[1].strip().rstrip("*/").strip() for line in head if TAG in line
        ]
        if licences != [want]:
            found = ", ".join(licences) or "none"
            problems.append(f"{path}: licence is {found!r}, its component's is {want!r}")
        if not any(COPYRIGHT in line for line in head):
            problems.append(f"{path}: missing '{COPYRIGHT}'")
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print(f"{len(problems)} problem(s)", file=sys.stderr)
        return 1
    print("every source file carries its component's licence")
    return 0


if __name__ == "__main__":
    sys.exit(main())
