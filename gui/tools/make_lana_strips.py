#!/usr/bin/env python3
"""Matte Lana's masters off their solid-black backgrounds and rebuild the
figure strips with real alpha.

Why: the five state masters (gui/assets/lana/masters/*.png, 640x1280) were
generated on solid black, so the shipped strips showed her in a black box.
The Owner wants her floating over the lab backdrop (2026-11-27). This tool
is the deterministic matte + strip rebuild; the masters stay untouched as
the sources of record.

Matte method (deterministic, no randomness, no ML):
  1. A pixel is a background *candidate* when max(r,g,b) <= CAND_MAX (46; the generated
     backgrounds are noisy near-black, border samples run to 38).
  2. Flood fill from every border pixel through candidates -> background.
     Interior dark detail (the black thong, pupils, shadowed folds) is not
     reachable from the border and stays opaque.
  3. Alpha: background 0, foreground 255. Edge feather: a foreground pixel
     adjacent to background whose brightness is below FEATHER_MAX (90)
     gets alpha scaled by its brightness between CAND_MAX..FEATHER_MAX —
     those pixels are figure/black blends, so partial alpha is the honest
     value. Colours are kept as-is (the background was black, so there is
     no colour spill to remove).
  4. Speck cleanup: background-labelled pixels fully enclosed by foreground
     (unreachable holes the flood fill could not have produced — none are
     expected) are left as labelled; unreachable *candidate* islands inside
     the figure stay opaque by construction of step 2.

Strips: each matted master is resized to the 144x288 cell (LANCZOS) and
laid out as 12 frames with the same deterministic breathing bob the app
already labels as its procedural placeholder: dy = round(sin(2*pi*i/12))
in {-1,0,1} px plus a +/-1 px vertical squash in phase. `lana.png` is the
matted idle master itself.

Usage: python3 tools/make_lana_strips.py   (run from gui/)
"""

from __future__ import annotations

import math
import sys
from collections import deque
from pathlib import Path

from PIL import Image

CAND_MAX = 46
FEATHER_MAX = 110
CELL_W, CELL_H = 144, 288
FRAMES = 12
STATES = ["idle", "guiding", "waiting", "success", "error"]


def matte(img: Image.Image) -> Image.Image:
    """Return an RGBA copy of `img` with the border-connected black
    background made transparent (see module docstring)."""
    rgb = img.convert("RGB")
    w, h = rgb.size
    px = rgb.load()
    bright = [[0] * w for _ in range(h)]
    for y in range(h):
        row = bright[y]
        for x in range(w):
            r, g, b = px[x, y]
            row[x] = max(r, g, b)

    bg = [[False] * w for _ in range(h)]
    q: deque[tuple[int, int]] = deque()

    def seed(x: int, y: int) -> None:
        if not bg[y][x] and bright[y][x] <= CAND_MAX:
            bg[y][x] = True
            q.append((x, y))

    for x in range(w):
        seed(x, 0)
        seed(x, h - 1)
    for y in range(h):
        seed(0, y)
        seed(w - 1, y)
    while q:
        x, y = q.popleft()
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < w and 0 <= ny < h:
                seed(nx, ny)

    out = Image.new("RGBA", (w, h))
    opx = out.load()
    for y in range(h):
        for x in range(w):
            r, g, b = px[x, y]
            if bg[y][x]:
                opx[x, y] = (r, g, b, 0)
                continue
            a = 255
            v = bright[y][x]
            if v < FEATHER_MAX and (
                (x > 0 and bg[y][x - 1])
                or (x < w - 1 and bg[y][x + 1])
                or (y > 0 and bg[y - 1][x])
                or (y < h - 1 and bg[y + 1][x])
            ):
                a = round(255 * (v - CAND_MAX) / (FEATHER_MAX - CAND_MAX))
                a = max(0, min(255, a))
            opx[x, y] = (r, g, b, a)
    return out


def build_strip(master_rgba: Image.Image) -> Image.Image:
    cell = master_rgba.resize((CELL_W, CELL_H), Image.LANCZOS)
    strip = Image.new("RGBA", (CELL_W * FRAMES, CELL_H), (0, 0, 0, 0))
    for i in range(FRAMES):
        phase = 2 * math.pi * i / FRAMES
        dy = round(math.sin(phase))  # -1, 0 or 1 px bob
        squash = round(math.sin(phase + math.pi / 2))  # +/-1 px, in quadrature
        frame = cell.resize((CELL_W, CELL_H + squash), Image.LANCZOS) \
            if squash else cell
        strip.paste(frame, (i * CELL_W, dy), frame)
    return strip


def main() -> int:
    gui = Path(__file__).resolve().parent.parent
    lana = gui / "assets" / "lana"
    for state in STATES:
        src = lana / "masters" / f"{state}.png"
        matted = matte(Image.open(src))
        strip = build_strip(matted)
        strip.save(lana / f"{state}.png")
        if state == "idle":
            matted.save(lana / "lana.png")
        cov = sum(1 for a in matted.getchannel("A").getdata() if a > 0)
        total = matted.size[0] * matted.size[1]
        print(f"{state}: matted, foreground {cov}/{total} "
              f"({100.0 * cov / total:.1f}%), strip rebuilt")
    return 0


if __name__ == "__main__":
    sys.exit(main())
