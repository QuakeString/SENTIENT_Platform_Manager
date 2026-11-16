#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Overflow check for one render: nothing may reach into a card's right
padding or the page's right margin.

    check.py <screenshot.png> <stderr log> <scale>

The log holds the cards' bounds (logical px) as `LAYOUT panel l r t b`
lines after the last `LAYOUT-FRAME w h` mark (SPM_DEBUG_LAYOUT=1). For each
card on screen, the strip inside its right border (the 18 px padding, minus
2 px either side) must be the card's own colour; the page's right margin (24
px) must be the page's colour. Prints one line per finding and exits 1 if
there is any.
"""
import sys
from PIL import Image

PAD, PAGE_PAD, TOL, MIN_PIXELS = 18, 24, 28, 3


def frame_panels(log):
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return None, []
    # The last complete frame: from the last mark but one when the last
    # frame may still be printing, else the last mark.
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    w, h = (float(x) for x in lines[start].split()[1:3])
    panels = []
    for l in lines[start + 1:end]:
        p = l.split()
        if len(p) == 6 and p[0] == "LAYOUT" and p[1] == "panel":
            panels.append(tuple(float(x) for x in p[2:]))
    return (w, h), sorted(set(panels))


def differs(a, b):
    return max(abs(a[i] - b[i]) for i in range(3)) > TOL


def main():
    png, log, scale = sys.argv[1], sys.argv[2], float(sys.argv[3])
    im = Image.open(png).convert("RGB")
    W, H = im.size
    size, panels = frame_panels(log)
    if size is None:
        print(f"{png}: no layout lines (SPM_DEBUG_LAYOUT not on?)")
        return 1
    bad = []
    top_bar = 42 * scale
    for (l, r, t, b) in panels:
        L, R, T, B = (v * scale for v in (l, r, t, b))
        if R <= 0 or L >= W or B <= top_bar or T >= H or R - L < 80 * scale:
            continue
        y0, y1 = int(max(T + 14 * scale, top_bar + 2)), int(min(B - 14 * scale, H - 2))
        x0, x1 = int(R - (PAD - 2) * scale), int(R - 3 * scale)
        if y1 <= y0 or x1 <= x0 or x1 >= W:
            continue
        ref = im.getpixel((int(R - PAD * scale / 2), y0))
        hits = [(x, y) for y in range(y0, y1, 2) for x in range(x0, x1) if differs(im.getpixel((x, y)), ref)]
        if len(hits) >= MIN_PIXELS:
            x, y = hits[0]
            bad.append(f"card {l:.0f}-{r:.0f} x {t:.0f}-{b:.0f}: {len(hits)} px in its right padding, first at ({x / scale:.0f}, {y / scale:.0f})")
    # The page's right margin, below the top bar.
    x0, x1 = int(W - (PAGE_PAD - 3) * scale), int(W - 3 * scale)
    ref = im.getpixel((W - int(PAGE_PAD * scale / 2), H - int(6 * scale)))
    hits = [(x, y) for y in range(int(top_bar + 60 * scale), H - int(4 * scale), 2) for x in range(x0, x1) if differs(im.getpixel((x, y)), ref)]
    if len(hits) >= MIN_PIXELS:
        x, y = hits[0]
        bad.append(f"page margin: {len(hits)} px, first at ({x / scale:.0f}, {y / scale:.0f})")
    for b in bad:
        print(f"{png.split('/')[-1]}: {b}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
