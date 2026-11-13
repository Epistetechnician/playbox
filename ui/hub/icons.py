"""PNG app icons drawn at request time with the stdlib (no image library, no binary files in the repo)."""
import functools
import math
import struct
import zlib

BG = (0x17, 0x17, 0x15)
LIT = (0xFF, 0x5A, 0x1F)
DIM = (0x2A, 0x2A, 0x26)
P_GLYPH = (30, 17, 17, 30, 16, 16, 16)      # the 5x7 dot-matrix "P", one bit per dot
PITCH, DOT, X0, Y0 = 52, 40, 132, 80


def _cov(d):
    return max(0.0, min(1.0, 0.5 - d))


@functools.lru_cache(maxsize=8)
def png_icon(size, maskable=False):
    k = 512 / size
    zoom = 0.78 if maskable else 1.0       # maskable icons keep their art inside the central safe zone
    rows = []
    for py in range(size):
        row = bytearray([0])
        for px in range(size):
            x, y = (px + .5) * k, (py + .5) * k
            if maskable:
                a = 1.0
            else:   # rounded square, radius 96
                qx, qy = abs(x - 256) - (256 - 96), abs(y - 256) - (256 - 96)
                d = (math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - 96) / k
                a = _cov(d)
            col = list(BG)
            cx, cy = 256 + (x - 256) / zoom, 256 + (y - 256) / zoom
            c = min(4, max(0, round((cx - X0 - DOT / 2) / PITCH)))
            r = min(6, max(0, round((cy - Y0 - DOT / 2) / PITCH)))
            qx, qy = abs(cx - (X0 + c * PITCH + DOT / 2)) - (DOT / 2 - 6), abs(cy - (Y0 + r * PITCH + DOT / 2)) - (DOT / 2 - 6)
            d = math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - 6
            t = _cov(d / k * zoom)
            if t:
                c2 = LIT if P_GLYPH[r] >> (4 - c) & 1 else DIM
                col = [col[i] * (1 - t) + c2[i] * t for i in range(3)]
            row += bytes((int(col[0]), int(col[1]), int(col[2]), int(a * 255)))
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
