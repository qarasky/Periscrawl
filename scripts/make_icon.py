#!/usr/bin/env python3
"""Generate the Periscrawl app icon / logo (PNG) with no external dependencies.

The mark is a periscope scanning a small grid of pages: a steel periscope with
a cyan lens, a fading scan trail, and a 3x3 page grid where one page is
highlighted coral (a found issue). Rendered from signed-distance functions
with smoothstep anti-aliasing.

Usage:
    python3 scripts/make_icon.py OUTPUT.png [SIZE]
"""
import math
import struct
import sys
import zlib

BASE = 1024.0

# Palette
BG = (26, 34, 56)            # deep navy tile
STEEL = (226, 234, 246)      # periscope body
LENS = (86, 199, 255)        # scanning cyan
GLINT = (240, 248, 255)      # lens highlight
PAGE_DIM = (96, 120, 165)    # unscanned / normal pages
PAGE_ISSUE = (255, 122, 89)  # the page with a problem


def smooth(edge_out: float, edge_in: float, x: float) -> float:
    """Coverage for a signed distance: 1 deep inside (< edge_in), 0 outside (> edge_out)."""
    t = min(max((x - edge_in) / (edge_out - edge_in), 0.0), 1.0)
    return 1.0 - t * t * (3.0 - 2.0 * t)


def sd_round_rect(px, py, hw, hh, radius):
    qx = abs(px) - (hw - radius)
    qy = abs(py) - (hh - radius)
    return math.hypot(max(qx, 0.0), max(qy, 0.0)) + min(max(qx, qy), 0.0) - radius


def sd_circle(px, py, cx, cy, r):
    return math.hypot(px - cx, py - cy) - r


def sd_segment(px, py, ax, ay, bx, by, r):
    abx, aby = bx - ax, by - ay
    apx, apy = px - ax, py - ay
    t = max(0.0, min(1.0, (apx * abx + apy * aby) / (abx * abx + aby * aby)))
    return math.hypot(apx - t * abx, apy - t * aby) - r


def blend(dst, src, alpha):
    alpha = max(0.0, min(1.0, alpha))
    return tuple(int(round(d * (1 - alpha) + s * alpha)) for d, s in zip(dst[:3], src)) + (255,)


def render(size: int) -> bytes:
    s = size / BASE
    c = size / 2.0
    half = c - 20.0 * s

    # Periscope: vertical tube + horizontal head (union forms the L shape).
    tube = lambda px, py: sd_round_rect(px - 400.0 * s, py - 590.0 * s, 75.0 * s, 260.0 * s, 75.0 * s)
    head = lambda px, py: sd_round_rect(px - 492.0 * s, py - 405.0 * s, 168.0 * s, 75.0 * s, 75.0 * s)

    lens_cx, lens_cy = 592.0 * s, 405.0 * s
    ring_mid, ring_half = 56.0 * s, 9.0 * s
    disc_r = 40.0 * s
    glint = (572.0 * s, 386.0 * s, 12.0 * s)

    # Scan trail: dashes flying from the lens toward the page grid.
    dashes = [
        (652.0, 468.0, 666.0, 494.0, 0.85),
        (676.0, 514.0, 690.0, 540.0, 0.55),
        (700.0, 560.0, 714.0, 586.0, 0.30),
    ]

    # 3x3 page grid; the middle-right page is the highlighted "issue".
    pages = []
    for iy in range(3):
        for ix in range(3):
            cx, cy = 700.0 + ix * 100.0, 600.0 + iy * 100.0
            issue = (ix == 1 and iy == 1)
            pages.append((cx, cy, issue))

    rows = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            px, py = x + 0.5, y + 0.5
            alpha_bg = smooth(1.6 * s, -0.8 * s, sd_round_rect(px - c, py - c, half, half, 190.0 * s))
            if alpha_bg <= 0.0:
                continue
            color = BG + (255,)

            glyph = smooth(1.6 * s, -1.6 * s, min(tube(px, py), head(px, py)))
            if glyph > 0:
                color = blend(color, STEEL, glyph * alpha_bg)

            ring = smooth(1.4 * s, -1.4 * s, abs(sd_circle(px, py, lens_cx, lens_cy, ring_mid)) - ring_half)
            if ring > 0:
                color = blend(color, STEEL, ring * alpha_bg)

            disc = smooth(1.4 * s, -1.4 * s, sd_circle(px, py, lens_cx, lens_cy, disc_r))
            if disc > 0:
                color = blend(color, LENS, disc * alpha_bg)

            spot = smooth(1.2 * s, -1.2 * s, sd_circle(px, py, glint[0], glint[1], glint[2]))
            if spot > 0:
                color = blend(color, GLINT, spot * 0.85 * alpha_bg)

            for ax, ay, bx, by, strength in dashes:
                d = smooth(1.2 * s, -1.2 * s, sd_segment(px, py, ax * s, ay * s, bx * s, by * s, 12.0 * s))
                if d > 0:
                    color = blend(color, LENS, d * strength * alpha_bg)

            for cx, cy, issue in pages:
                sq = smooth(1.4 * s, -1.4 * s, sd_round_rect(px - cx * s, py - cy * s, 33.0 * s, 33.0 * s, 15.0 * s))
                if sq > 0:
                    color = blend(color, PAGE_ISSUE if issue else PAGE_DIM, sq * alpha_bg)

            offset = (y * size + x) * 4
            rows[offset : offset + 4] = bytes(color)
        if y % 128 == 0:
            print(f"  … {y}/{size}", file=sys.stderr)

    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter: none
        raw += rows[y * size * 4 : (y + 1) * size * 4]

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    return png


def main(path: str, size: int = 1024) -> None:
    png = render(size)
    with open(path, "wb") as handle:
        handle.write(png)
    print(f"wrote {path} ({size}x{size})", file=sys.stderr)


if __name__ == "__main__":
    output = sys.argv[1] if len(sys.argv) > 1 else "icon.png"
    dim = int(sys.argv[2]) if len(sys.argv) > 2 else 1024
    main(output, dim)
