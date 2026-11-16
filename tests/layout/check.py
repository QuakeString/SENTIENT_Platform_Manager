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

PAD, PAGE_PAD, TOL, MIN_PIXELS = 16, 18, 28, 3


def frame_panels(log):
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return None, [], False
    # The last complete frame: from the last mark but one when the last
    # frame may still be printing, else the last mark.
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    w, h = (float(x) for x in lines[start].split()[1:3])
    panels = []
    dialog = False
    for l in lines[start + 1:end]:
        p = l.split()
        # Cards, and dialogs' bodies (both keep 18-20 px inside their edge).
        if len(p) == 6 and p[0] == "LAYOUT" and p[1] in ("panel", "dialog", "overlay"):
            panels.append((p[1],) + tuple(float(x) for x in p[2:]))
            dialog = dialog or p[1] == "dialog"
    panels = sorted(set(panels))
    # A dialog open: only its body counts (the page under it is covered).
    # A floating panel (the bell, a popover): the cards it covers are skipped.
    overlays = [q[1:] for q in panels if q[0] == "overlay"]
    def covered(c):
        return any(not (c[1] <= o[0] or c[0] >= o[1] or c[3] <= o[2] or c[2] >= o[3]) for o in overlays)
    if dialog:
        keep = [q[1:] for q in panels if q[0] == "dialog"]
    else:
        keep = [q[1:] for q in panels if q[0] == "panel" and not covered(q[1:])]
    return (w, h), keep, dialog or bool(overlays)


def menu_selection(log):
    lines = open(log, errors="replace").read().splitlines()
    sels = [l.split()[2:] for l in lines if l.startswith("LAYOUT menu-sel ")]
    return tuple(float(x) for x in sels[-1]) if sels else None


def bar_controls(log):
    """The top bar's controls of the last complete frame (switcher, monitors,
    theme, bell): their heights must be one."""
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return []
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    return [tuple(float(x) for x in l.split()[2:]) for l in lines[start + 1:end] if l.startswith("LAYOUT bar-ctrl ")]


def differs(a, b):
    return max(abs(a[i] - b[i]) for i in range(3)) > TOL


def main():
    png, log, scale = sys.argv[1], sys.argv[2], float(sys.argv[3])
    im = Image.open(png).convert("RGB")
    W, H = im.size
    size, panels, dialog = frame_panels(log)
    if size is None:
        print(f"{png}: no layout lines (SPM_DEBUG_LAYOUT not on?)")
        return 1
    bad = []
    # The bar over the page (the shell's 60 px).
    top_bar = 60 * scale
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
        # A scrolling body's scrollbar track: one or two full-height columns
        # of the same line, not content.
        cols = {x for x, _ in hits}
        rows = len(range(y0, y1, 2))
        if hits and len(cols) <= 2 * max(1, int(scale)) and len(hits) >= 0.8 * rows * len(cols):
            hits = []
        if len(hits) >= MIN_PIXELS:
            x, y = hits[0]
            bad.append(f"card {l:.0f}-{r:.0f} x {t:.0f}-{b:.0f}: {len(hits)} px in its right padding, first at ({x / scale:.0f}, {y / scale:.0f})")
    # The menu's selected row opens into the page: no edge line between
    # its curves (the page's colour all across the menu's edge there).
    sel = menu_selection(log)
    if sel and not dialog:
        l, edge, t, b = sel
        ref = im.getpixel((int(edge * scale + 6 * scale), int((t + b) / 2 * scale)))
        ys = range(int((t + 3) * scale), int((b - 3) * scale))
        xs = range(int(edge * scale) - 3, int(edge * scale) + 3)
        hits = [(x, y) for y in ys for x in xs if 0 <= x < W and differs(im.getpixel((x, y)), ref)]
        if len(hits) >= MIN_PIXELS:
            x, y = hits[0]
            bad.append(f"menu edge: {len(hits)} px of line inside the selected row's opening, first at ({x / scale:.1f}, {y / scale:.1f})")
    # One height for the top bar's controls (and centred in the bar).
    ctrls = bar_controls(log)
    if ctrls:
        hs = sorted({round(b - t) for (l, r, t, b) in ctrls})
        if len(hs) > 1:
            bad.append(f"top bar controls: heights {hs} differ")
        mids = sorted({round((t + b) / 2) for (l, r, t, b) in ctrls})
        if len(mids) > 1:
            bad.append(f"top bar controls: not on one centre line {mids}")
    # The bar's own right padding (16 px): nothing in it at any width.
    bx0, bx1 = int(W - 13 * scale), int(W - 3 * scale)
    bref = im.getpixel((W - int(8 * scale), int(4 * scale)))
    bhits = [(x, y) for y in range(int(6 * scale), int(top_bar - 6 * scale), 2) for x in range(bx0, bx1) if differs(im.getpixel((x, y)), bref)]
    if len(bhits) >= MIN_PIXELS:
        x, y = bhits[0]
        bad.append(f"bar edge: {len(bhits)} px in its right padding, first at ({x / scale:.0f}, {y / scale:.0f})")
    # The page's right margin, below the top bar (not with a dialog open:
    # a wide one covers it).
    if dialog:
        for b in bad:
            print(f"{png.split('/')[-1]}: {b}")
        return 1 if bad else 0
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
