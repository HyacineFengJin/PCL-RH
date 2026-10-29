#!/usr/bin/env python3
"""Wrap the original 256px brand PNG in a Windows ICO without changing pixels.

Run from any directory: python3 tools/app-icons/build.py
The source remains authoritative; the ICO payload is exactly the same PNG.
"""
from pathlib import Path
import struct


def main():
    assets = Path(__file__).resolve().parents[2] / "assets"
    png = (assets / "pcl-rh.png").read_bytes()
    if (len(png) < 33 or png[:8] != b"\x89PNG\r\n\x1a\n"
            or png[12:16] != b"IHDR"
            or struct.unpack(">II", png[16:24]) != (256, 256)
            or tuple(png[24:26]) != (8, 6)):
        raise SystemExit("Expected the original 256x256 RGBA PNG")
    # Width/height 0 encode 256 in the one-entry ICO directory.
    directory = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22)
    (assets / "pcl-rh.ico").write_bytes(directory + entry + png)


if __name__ == "__main__":
    main()
