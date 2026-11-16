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
import os
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


PAGE_GAP = 18.0


def page_gaps(log, size, panels):
    """The page's gaps (theme::PAGE_GAP): the menu edge to the cards, the
    cards to the window's right edge, and card to card down a column and
    across a row. Returns (what, logical px) pairs that are off."""
    lines = open(log, errors="replace").read().splitlines()
    edges = [float(l.split()[2]) for l in lines if l.startswith("LAYOUT menu-edge ")]
    if not edges or not panels:
        return []
    menu = edges[-1]
    w = size[0]
    # A card's probe is inside its 1 px border: its outer box is 1 px more.
    panels = [(l - 1, r + 1, t - 1, b + 1) for (l, r, t, b) in panels]
    # The page's own cards: those not inside another card.
    def inside(a, b):
        return a != b and a[0] >= b[0] - 0.5 and a[1] <= b[1] + 0.5 and a[2] >= b[2] - 0.5 and a[3] <= b[3] + 0.5
    top = [p for p in panels if not any(inside(p, q) for q in panels)]
    off = []
    left = min(p[0] for p in top)
    right = max(p[1] for p in top)
    # A page whose content is centred (a set width, as About or Licence)
    # keeps equal sides instead.
    centred = abs((left - menu) - (w - right)) <= 1 and left - menu > PAGE_GAP + 1
    # A page with a column of its own on the left (Platform settings' groups)
    # starts its cards further in.
    side_column = left - menu > 3 * PAGE_GAP
    if not centred and not side_column:
        if abs(left - menu - PAGE_GAP) > 1:
            off.append(("menu edge to cards", left - menu))
        if abs(w - right - PAGE_GAP) > 1:
            off.append(("cards to the window edge", w - right))
    for a in top:
        # The nearest card below, overlapping in x: the gap down.
        below = [b for b in top if b[2] >= a[3] - 0.5 and not (b[1] <= a[0] or b[0] >= a[1])]
        if below:
            g = min(b[2] for b in below) - a[3]
            # A section heading between two cards (About's APPEARANCE) is
            # content, not a gap.
            if abs(g - PAGE_GAP) > 1 and g < 2 * PAGE_GAP:
                off.append((f"card to card down at y {a[3]:.0f}", g))
        # The nearest card to the right, overlapping in y: the gap across.
        beside = [b for b in top if b[0] >= a[1] - 0.5 and not (b[3] <= a[2] or b[2] >= a[3])]
        if beside:
            g = min(b[0] for b in beside) - a[1]
            if abs(g - PAGE_GAP) > 1:
                off.append((f"card to card across at x {a[1]:.0f}", g))
    return off


def differs(a, b):
    return max(abs(a[i] - b[i]) for i in range(3)) > TOL


def broken_dividers(im, panels, scale, top_bar):
    """A card's horizontal row dividers are one continuous line each (his
    rc12-22 note: a line per column left a gap before the action column).
    A divider is a pixel row inside the card where at least half of the
    inner width is one flat colour other than the card's; inside its first
    and last pixel there may be no gap of 6 px or more of card colour."""
    W, H = im.size
    out = []
    for (l, r, t, b) in panels:
        L, R, T, B = (v * scale for v in (l, r, t, b))
        if B <= top_bar or T >= H or R - L < 160 * scale:
            continue
        x0, x1 = int(L + PAD * scale), int(min(R - PAD * scale, W - 1))
        span = x1 - x0
        if span < 100:
            continue
        gap = 6 * scale
        for y in range(int(max(T + 4 * scale, top_bar + 2)), int(min(B - 4 * scale, H - 1))):
            bg = im.getpixel((int(L + 6 * scale), y))
            px = [im.getpixel((x, y)) for x in range(x0, x1)]
            # Dividers are faint (the theme border over the card): a finer
            # threshold than the overflow check's.
            near = lambda c, d: max(abs(a - b) for a, b in zip(c, d)) <= 6
            off = [i for i, c in enumerate(px) if not near(c, bg)]
            if len(off) < span * 0.5:
                continue
            # One flat colour: the most common differing colour covers most.
            from collections import Counter
            colour, n = Counter(px[i] for i in off).most_common(1)[0]
            if n < len(off) * 0.8:
                continue
            on = [i for i in off if max(abs(a - b) for a, b in zip(px[i], colour)) <= 6]
            # Thin: a few pixels above and below are the card's own colour
            # (a filled banner or a field is not a divider).
            d = max(2, int(3 * scale))
            if y - d < 0 or y + d >= H:
                continue
            sample = on[:: max(1, len(on) // 40)]
            thin = sum(1 for i in sample if near(im.getpixel((x0 + i, y - d)), bg) and near(im.getpixel((x0 + i, y + d)), bg))
            if thin < len(sample) * 0.7:
                continue
            run_gap, worst, at = 0, 0, None
            for i in range(on[0], on[-1] + 1):
                if not near(px[i], bg):
                    run_gap = 0
                else:
                    run_gap += 1
                    if run_gap > worst:
                        worst, at = run_gap, i
            if worst >= gap:
                out.append(f"card {l:.0f}-{r:.0f}: row divider at y {y / scale:.0f} broken by {worst / scale:.0f} px near x {(x0 + at) / scale:.0f}")
                break
    return out


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
    # Row dividers: one continuous line per row.
    if not dialog:
        bad.extend(broken_dividers(im, panels, scale, top_bar))
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
    # One page gap everywhere (not with a dialog or a floating panel open).
    if not dialog and os.environ.get("SPM_CHECK_GAPS", "1") == "1":
        for what, g in page_gaps(log, size, panels):
            bad.append(f"page gap: {what} is {g:.1f} px, not {PAGE_GAP:.0f}")
    # Menu icons (an open popup or context menu): at least 18 px.
    for l in open(log, errors="replace").read().splitlines():
        if l.startswith("LAYOUT menu-item-icon "):
            x0, x1, y0, y1 = (float(v) for v in l.split()[2:])
            if x1 - x0 < 17.5 or y1 - y0 < 17.5:
                bad.append(f"menu icon {x1 - x0:.1f}x{y1 - y0:.1f} px, under 18")
                break
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
