#!/usr/bin/env python3
"""Draw the CalLink icon (256x256 RGBA PNG) using only the standard library.

A calculator body with an LCD showing two chain links, and a keypad.
Usage: python3 make_icon.py OUT.png [OUT.ico]
"""
import struct
import sys
import zlib

SIZE = 256
SS = 4  # supersampling per axis


def rrect_dist(x, y, x0, y0, x1, y1, r):
    """Signed distance to a rounded rectangle (negative inside)."""
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    hx, hy = (x1 - x0) / 2 - r, (y1 - y0) / 2 - r
    dx, dy = abs(x - cx) - hx, abs(y - cy) - hy
    ox, oy = max(dx, 0.0), max(dy, 0.0)
    return (ox * ox + oy * oy) ** 0.5 + min(max(dx, dy), 0.0) - r


BODY = (36, 44, 56)
SCREEN = (180, 194, 166)
INK = (34, 56, 46)
KEY = (70, 82, 98)
ACCENT = (64, 146, 226)

# (kind, geometry, colour) painted in order
SHAPES = [("fill", (44, 12, 212, 244, 26), BODY), ("fill", (66, 36, 190, 114, 12), SCREEN)]
# chain links on the LCD (outlines)
SHAPES += [("stroke", (84, 62, 136, 90, 14, 5.0), INK), ("stroke", (120, 62, 172, 90, 14, 5.0), INK)]
# keypad 4x4
for row in range(4):
    for col in range(4):
        x0, y0 = 66 + col * 32, 130 + row * 27
        colour = ACCENT if (row, col) == (3, 3) else KEY
        SHAPES.append(("fill", (x0, y0, x0 + 26, y0 + 19, 6), colour))


def sample(x, y):
    colour, alpha = (0, 0, 0), 0.0
    for kind, g, c in SHAPES:
        if kind == "fill":
            inside = rrect_dist(x, y, *g) <= 0
        else:
            *rect, w = g
            inside = abs(rrect_dist(x, y, *rect)) <= w / 2
        if inside:
            colour, alpha = c, 1.0
    return colour, alpha


def render():
    rows = []
    for py in range(SIZE):
        row = bytearray([0])  # PNG filter: none
        for px in range(SIZE):
            r = g = b = a = 0.0
            for sy in range(SS):
                for sx in range(SS):
                    (cr, cg, cb), ca = sample(px + (sx + 0.5) / SS, py + (sy + 0.5) / SS)
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            n = SS * SS
            if a:
                row += bytes([round(r / a), round(g / a), round(b / a), round(255 * a / n)])
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return b"".join(rows)


def png(raw):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(png_bytes):
    """A Windows .ico holding the 256x256 PNG (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png_bytes), 6 + 16)
    return header + entry + png_bytes


if __name__ == "__main__":
    data = png(render())
    with open(sys.argv[1], "wb") as f:
        f.write(data)
    if len(sys.argv) > 2:  # optional .ico for Windows
        with open(sys.argv[2], "wb") as f:
            f.write(ico(data))
