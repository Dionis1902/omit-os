#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Omit OS contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Renders the glyphs the desktop uses from the Inter font into the
small bitmap-font files in libs/gfx/fonts/ (format: libs/gfx/src/font.rs).

The outputs are committed, so building the system needs neither this
script nor the font; run it again only to change sizes or characters.

    python3 tools/gen-font.py /usr/share/fonts/opentype/inter libs/gfx/fonts

Needs Pillow. The font is Inter 4.0 (SIL Open Font License 1.1); the
generated files are a rendering of it and stay under that licence.
"""

import struct
import sys

from PIL import Image, ImageDraw, ImageFont

# Printable ASCII, and the few other characters the interface uses.
CHARACTERS = [chr(c) for c in range(32, 127)] + list("…–—·×›‹•’‘“”°→←↑↓✓")
# (file name, font file, size in pixels)
FACES = [
    ("inter-regular-11", "Inter-Regular.otf", 11),
    ("inter-regular-12", "Inter-Regular.otf", 12),
    ("inter-regular-13", "Inter-Regular.otf", 13),
    ("inter-medium-13", "Inter-Medium.otf", 13),
    ("inter-semibold-13", "Inter-SemiBold.otf", 13),
    ("inter-semibold-15", "Inter-SemiBold.otf", 15),
    ("inter-semibold-20", "Inter-SemiBold.otf", 20),
]


def render(font_path, size):
    font = ImageFont.truetype(font_path, size)
    ascent, descent = font.getmetrics()
    records = []
    coverage = bytearray()
    # Sorted by code point: the reader searches by halving.
    for character in sorted(set(CHARACTERS)):
        advance = round(font.getlength(character))
        box = font.getbbox(character)  # relative to the pen, y from the ascent line
        if box is None or box[2] <= box[0] or box[3] <= box[1]:
            records.append((ord(character), advance, 0, 0, 0, 0, len(coverage)))
            continue
        left, top, right, bottom = box
        width, height = right - left, bottom - top
        image = Image.new("L", (width, height), 0)
        ImageDraw.Draw(image).text((-left, -top), character, font=font, fill=255)
        records.append((ord(character), advance, left, top, width, height, len(coverage)))
        coverage += image.tobytes()
    # Header: magic, size, ascent, descent, glyph count.
    out = bytearray(b"OMF1")
    out += struct.pack("<BBBxH2x", size, ascent, descent, len(records))
    for code, advance, left, top, width, height, offset in records:
        assert 0 <= advance < 256 and -128 <= left < 128 and -128 <= top < 128
        assert width < 256 and height < 256
        out += struct.pack("<IBbbBBxxxI", code, advance, left, top, width, height, offset)
    out += coverage
    return bytes(out)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    source, destination = sys.argv[1], sys.argv[2]
    for name, file, size in FACES:
        data = render(f"{source}/{file}", size)
        with open(f"{destination}/{name}.omf", "wb") as out:
            out.write(data)
        print(f"{name}.omf: {len(data)} bytes")


if __name__ == "__main__":
    main()
