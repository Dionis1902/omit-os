#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Keeps other vendors' branding out of the system (CONTRIBUTING.md,
"Names and assets").

1. No file may use another vendor's branded feature name ("Snap
   Layouts", "Mission Control", ...). Our features get plain names.
2. No file may claim an identity or endorsement that is not ours
   ("Windows clone", "by Microsoft", ...).
3. Every asset file (font, image, icon, cursor, sound) is listed in
   ATTRIBUTION.md.

Exempt from 1 and 2: the documents that have to name these things in
order to rule them out, and — for rule 1 only — nothing else: the
Windows compatibility code needs Windows *API* names (DLLs, functions,
registry paths), none of which is on the list below, so it gets no
blanket exemption. If an API identifier ever collides with the list,
add that exact identifier to API_IDENTIFIERS.

    python3 tools/check-branding.py
"""

import re
import subprocess
import sys

# Branded feature names of other vendors, and what to call the feature.
FEATURE_NAMES = {
    r"snap layouts?": "Arrange",
    r"snap assist": "Arrange",
    r"control cent(er|re)": "Quick panel",
    r"action cent(er|re)": "Notifications",
    r"notification cent(er|re)": "Notifications",
    r"windows update": "Updates",
    r"mission control": "a plain description (e.g. Overview)",
    r"spotlight": "Search",
    r"launchpad": "a plain description (e.g. Programs)",
    r"cortana": "a plain description (e.g. Assistant)",
    r"copilot": "a plain description (e.g. Assistant)",
    r"task view": "a plain description (e.g. Overview)",
    r"live tiles?": "a plain description",
    r"start menu": "Start",
    r"file explorer|windows explorer": "Files",
    r"finder": "Files",
    r"the dock": "Taskbar",
    r"airdrop|time machine|quick look": "a plain description",
}

# Wording that claims to be, or to be blessed by, someone else.
IDENTITY = [
    r"windows clone",
    r"clone of windows",
    r"windows replacement",
    r"replacement for windows",
    r"mac ?os clone",
    # "documented by Microsoft" is a fact about a source; "an OS by
    # Microsoft" is a claim about us.
    r"(os|system|product|software|made|built|developed|created|published) by (microsoft|apple)",
    r"(endorsed|sponsored|certified) by (microsoft|apple)",
    r"microsoft omit",
    r"omit os for windows",
]

# Files that name these things in order to forbid or disclaim them.
EXEMPT = (
    "README.md",
    "TRADEMARKS.md",
    "ATTRIBUTION.md",
    "CONTRIBUTING.md",
    "IP_POLICY.md",
    "tools/check-branding.py",
)
# Code we do not write: its text is upstream's.
THIRD_PARTY = ("drivers/linux/", "LICENSES/", "Cargo.lock")

# Windows API/ABI identifiers that contain a listed phrase and are
# therefore allowed, in the compatibility code only. Exact strings.
API_IDENTIFIERS = ()
COMPAT = ("compat/windows/", "userspace/programs/src/bin/winrun/")

ASSET_SUFFIXES = (
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".svg", ".ico", ".icns",
    ".cur", ".ani", ".ttf", ".otf", ".ttc", ".woff", ".woff2", ".fon", ".pcf",
    ".bdf", ".psf", ".wav", ".mp3", ".ogg", ".flac", ".mid", ".exe", ".dll",
    # This project's own formats for ready-drawn fonts and icons.
    ".omf", ".omi", ".omp",
)  # fmt: skip
TEXT_LIMIT = 2 << 20


def main():
    root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    ).stdout.strip()
    files = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True, cwd=root
    ).stdout.split("\n")
    with open(f"{root}/ATTRIBUTION.md", encoding="utf-8") as listing:
        attribution = listing.read()

    features = [(re.compile(rf"\b(?:{p})\b", re.I), fix) for p, fix in FEATURE_NAMES.items()]
    identity = [re.compile(rf"\b(?:{p})\b", re.I) for p in IDENTITY]
    problems = []
    for path in filter(None, files):
        if path.lower().endswith(ASSET_SUFFIXES):
            if f"`{path}`" not in attribution:
                problems.append(f"{path}: asset file without a row in ATTRIBUTION.md")
            continue
        if path in EXEMPT or path.startswith(THIRD_PARTY):
            continue
        try:
            with open(f"{root}/{path}", encoding="utf-8") as source:
                text = source.read(TEXT_LIMIT)
        except (UnicodeDecodeError, OSError):
            continue
        if path.startswith(COMPAT):
            for identifier in API_IDENTIFIERS:
                text = text.replace(identifier, "")
        for number, line in enumerate(text.split("\n"), 1):
            for pattern, fix in features:
                if found := pattern.search(line):
                    problems.append(
                        f"{path}:{number}: '{found.group(0)}' is another vendor's "
                        f"feature name; use {fix}"
                    )
            for pattern in identity:
                if found := pattern.search(line):
                    problems.append(
                        f"{path}:{number}: '{found.group(0)}' suggests an identity "
                        "or endorsement that is not ours"
                    )
    if problems:
        print("\n".join(problems))
        print(f"\n{len(problems)} problem(s); see CONTRIBUTING.md, 'Names and assets'")
        return 1
    print("no borrowed feature names or claims; every asset file is attributed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
