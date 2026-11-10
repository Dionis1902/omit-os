#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Writes docs/TRUSTED_AUTHORITIES.md: the certificate authorities that
encrypted connections trust by default, from the source of the
`webpki-roots` crate that Cargo has downloaded (run `cargo fetch` first).

    python3 tools/gen-authorities.py

Run it again whenever the crate's version in Cargo.lock changes.
"""

import glob
import os
import re
import sys

HEAD = """# Certificate authorities trusted by default

Encrypted connections made with `omit-tls` (the `fetch` program) accept
a server whose certificate leads to one of these authorities. The list
is Mozilla's — the authorities Mozilla's products trust for web sites —
as packaged by the `webpki-roots` crate, version {version}; it changes when
that dependency is updated, and a test in `userspace/tls` fails if this
page was not regenerated then (`python3 tools/gen-authorities.py`).

The data is made available under the Community Data License Agreement –
Permissive – Version 2.0 (`LICENSES/CDLA-Permissive-2.0.txt`). Omit OS
does not choose or vet these authorities, and has no way yet to learn
that a certificate was revoked.

| Authority (Mozilla's label) | SHA-256 fingerprint of its certificate |
| --- | --- |
"""


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    lock = open(os.path.join(root, "Cargo.lock")).read()
    found = re.search(r'name = "webpki-roots"\nversion = "([^"]+)"', lock)
    if not found:
        sys.exit("webpki-roots is not in Cargo.lock")
    version = found.group(1)
    home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    sources = glob.glob(
        os.path.join(home, "registry", "src", "*", f"webpki-roots-{version}", "src", "lib.rs")
    )
    if not sources:
        sys.exit(f"the source of webpki-roots {version} is not downloaded: run cargo fetch")
    source = open(sources[0]).read()
    entries = re.findall(
        r'\* Label: "(.*)"\n\s*\* Serial: .*\n\s*\* SHA256 Fingerprint: ([0-9a-f:]+)', source
    )
    rows = "".join(
        f"| {label} | `{fingerprint}` |\n"
        for label, fingerprint in sorted(entries, key=lambda entry: entry[0].lower())
    )
    with open(os.path.join(root, "docs", "TRUSTED_AUTHORITIES.md"), "w") as out:
        out.write(HEAD.format(version=version) + rows)
    print(f"{len(entries)} authorities, webpki-roots {version}")


if __name__ == "__main__":
    main()
