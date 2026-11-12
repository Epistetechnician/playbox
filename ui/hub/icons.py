"""PNG app icons drawn at request time with the stdlib (no image library, no binary files in the repo)."""
import functools
import math
import struct
import zlib

BG = (0x12, 0x12, 0x2A)
GOLD = (0xE8, 0xC8, 0x72)
INDIGO = (0xAA, 0xB4, 0xFF)
MINT = (0x7F, 0xD6, 0xB0)


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
            else:   # rounded square, radius 112
                qx, qy = abs(x - 256) - (256 - 112), abs(y - 256) - (256 - 112)
                d = (math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - 112) / k
                a = _cov(d)
            col = list(BG)
            cx, cy = 256 + (x - 256) / zoom, 256 + (y - 256) / zoom
            r = math.hypot(cx - 256, cy - 256)
            layers = [(abs(r - 150) - 7, GOLD, .35), (abs(r - 92) - 7, GOLD, .65), (r - 34, GOLD, 1.0),
                      (math.hypot(cx - 406, cy - 256) - 16, INDIGO, 1.0), (math.hypot(cx - 174, cy - 126) - 12, MINT, 1.0)]
            for dist, c, op in layers:
                t = _cov(dist / k * zoom) * op
                if t:
                    col = [col[i] * (1 - t) + c[i] * t for i in range(3)]
            row += bytes((int(col[0]), int(col[1]), int(col[2]), int(a * 255)))
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
