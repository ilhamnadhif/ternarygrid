#!/usr/bin/env python3
"""Builds a Windows .ico (16-256 px, PNG entries) from the app icon's SVG.

    python3 scripts/make-windows-icon.py <icon.svg> <out.ico>

Each size is rendered from the SVG with scripts/render-icon.swift (macOS), so small
sizes stay sharp. The .ico is committed: run scripts/make-icons.sh when the icon changes.
"""

import os
import struct
import subprocess
import sys
import tempfile

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
RENDER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "render-icon.swift")


def main():
    svg, out = sys.argv[1], sys.argv[2]
    images = []
    with tempfile.TemporaryDirectory() as tmp:
        for size in SIZES:
            path = os.path.join(tmp, f"{size}.png")
            subprocess.run(["swift", RENDER, svg, path, str(size)], check=True)
            with open(path, "rb") as f:
                images.append((size, f.read()))
    # ICONDIR, then one ICONDIRENTRY per image, then the PNG data.
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries, data = b"", b""
    for size, png in images:
        dim = 0 if size >= 256 else size  # 0 means 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    with open(out, "wb") as f:
        f.write(header + entries + data)
    print(f"{out}: {len(images)} sizes, {os.path.getsize(out)} bytes")


if __name__ == "__main__":
    main()
