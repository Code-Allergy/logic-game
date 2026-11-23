#!/usr/bin/env python3
"""Render a stored Screen memory (256 x 256, Rgb332, one byte per pixel)
to a PNG: python3 nes/gen/screen_png.py screen.bin out.png [rows]."""
import struct
import sys
import zlib

src, dst = sys.argv[1], sys.argv[2]
rows = int(sys.argv[3]) if len(sys.argv) > 3 else 240
d = open(src, "rb").read()
W = 256


def rgb(v):
    if v == 0:
        return (0, 0, 0)
    return ((v >> 5 & 7) * 255 // 7, (v >> 2 & 7) * 255 // 7, (v & 3) * 255 // 3)


raw = b"".join(b"\x00" + bytes(c for x in range(W) for c in rgb(d[y * W + x])) for y in range(rows))


def chunk(t, data):
    return struct.pack(">I", len(data)) + t + data + struct.pack(">I", zlib.crc32(t + data) & 0xFFFFFFFF)


open(dst, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, rows, 8, 2, 0, 0, 0))
                      + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))
