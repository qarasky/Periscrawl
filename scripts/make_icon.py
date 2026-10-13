#!/usr/bin/env python3
"""Generate the RustySEO app icon (PNG) with no external dependencies.

Draws a dark rounded square with a white magnifying glass over a small
"crawl graph" of accent dots. Anti-aliased via signed-distance smoothstep.
"""
import math
import struct
import sys
import zlib

SIZE = 1024
BG = (30, 42, 68)          # dark navy
GLYPH = (245, 248, 255)    # near-white
ACCENT = (90, 150, 255)    # RustySEO blue


def smooth(edge0: float, edge1: float, x: float) -> float:
    t = min(max((x - edge0) / (edge1 - edge0), 0.0), 1.0)
    return 1.0 - t * t * (3.0 - 2.0 * t)


def sd_round_rect(px, py, half, radius):
    qx = abs(px) - (half - radius)
    qy = abs(py) - (half - radius)
    return math.hypot(max(qx, 0.0), max(qy, 0.0)) + min(max(qx, qy), 0.0) - radius


def sd_circle(px, py, cx, cy, r):
    return math.hypot(px - cx, py - cy) - r


def sd_segment(px, py, ax, ay, bx, by, r):
    abx, aby = bx - ax, by - ay
    apx, apy = px - ax, py - ay
    t = max(0.0, min(1.0, (apx * abx + apy * aby) / (abx * abx + aby * aby)))
    return math.hypot(apx - t * abx, apy - t * aby) - r


def blend(dst, src, alpha):
    return tuple(int(round(d * (1 - alpha) + s * alpha)) for d, s in zip(dst[:3], src)) + (255,)


def main(path: str) -> None:
    c = SIZE / 2.0
    half = c - 20.0
    ring_cx, ring_cy = 430.0, 430.0
    ring_mid, ring_half = 165.0, 32.0
    handle = (560.0, 560.0, 745.0, 775.0, 34.0)
    dots = [(360.0, 350.0, 26.0), (500.0, 340.0, 24.0), (440.0, 470.0, 30.0)]

    rows = bytearray(SIZE * SIZE * 4)
    for y in range(SIZE):
        for x in range(SIZE):
            px, py = x + 0.5, y + 0.5
            alpha_bg = smooth(1.8, -0.8, sd_round_rect(px - c, py - c, half, 190.0))
            if alpha_bg <= 0.0:
                continue
            color = BG + (255,)
            alpha = alpha_bg

            glyph = max(
                smooth(1.6, -1.6, abs(math.hypot(px - ring_cx, py - ring_cy)) - ring_mid - ring_half),
                smooth(1.6, -1.6, abs(math.hypot(px - ring_cx, py - ring_cy)) - ring_mid + ring_half),
                smooth(1.6, -1.6, sd_segment(px, py, *handle)),
            )
            if glyph > 0:
                color = blend(color, GLYPH, glyph)

            dot_alpha = max(
                (smooth(1.6, -1.6, sd_circle(px, py, dx, dy, dr)) for dx, dy, dr in dots),
                default=0.0,
            )
            if dot_alpha > 0:
                color = blend(color, ACCENT, dot_alpha)

            offset = (y * SIZE + x) * 4
            rows[offset : offset + 4] = bytes(color)
        if y % 128 == 0:
            print(f"  … {y}/{SIZE}", file=sys.stderr)

    raw = bytearray()
    for y in range(SIZE):
        raw.append(0)  # filter: none
        raw += rows[y * SIZE * 4 : (y + 1) * SIZE * 4]

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as handle:
        handle.write(png)
    print(f"wrote {path} ({SIZE}x{SIZE})", file=sys.stderr)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "icon.png")
