#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Menu label sizes: a selected label is the same size as when it is not
selected (only its weight and colour change), and the rows keep one pitch.

    labels.py <out dir>

Reads every `<name>.log` + `<name>.png` render of run.sh in <out dir> whose
name ends in -x<scale>-<theme> (SPM_DEBUG_LAYOUT=1 logs with `LAYOUT
menu-label[-on] l r t b` and `LAYOUT menu-row l r t b`). For each label,
its ink height (cap or ascender top to descender bottom, sub-pixel from the
anti-aliasing, logical px) is measured in the screenshot. Renders of the
same width, scale and theme are grouped; a label's height when selected
must equal its height unselected within 0.5 px; at a scale that puts the
text between device pixels (x1.25, x1.75: scale * 2 not whole) within one
device pixel, since the semibold descender then rounds a pixel differently
from the medium one at the same font size (rc12-24, agreed with the owner). Rows: a 44 px pitch
(40 px rows, 4 px apart) below the brand row's 16 px clear space.
Prints findings and exits 1 if there are any.
"""
import os
import re
import sys
from collections import defaultdict
from PIL import Image

TOL = 0.5
ROW_H, PITCH, NAV_TOP = 40.0, 44.0, 16.0


def last_frame(log):
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return []
    s = marks[-2] if len(marks) > 1 else marks[-1]
    e = marks[-1] if len(marks) > 1 else len(lines)
    return lines[s + 1:e]


def lum(p):
    return 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]


def ink_height(im, box, sc):
    l, r, t, b = box
    x0, x1, y0, y1 = int(round(l * sc)), int(round(r * sc)), int(round(t * sc)), int(round(b * sc))
    x1 = min(x1, im.width); y1 = min(y1, im.height)
    if x1 - x0 < 2 or y1 - y0 < 2:
        return None
    bg = lum(im.getpixel((x0, y0)))
    cov = [max(abs(lum(im.getpixel((x, y))) - bg) for x in range(x0, x1)) for y in range(y0, y1)]
    peak = max(cov)
    if peak < 40:
        return None
    cov = [c / peak for c in cov]
    strong = [i for i, c in enumerate(cov) if c > 0.5]
    a, z = strong[0], strong[-1]
    # Whole rows inside, plus the edge rows' partial coverage.
    h = (z - a + 1) + (min(1.0, cov[a - 1] / 0.9) if a > 0 else 0) + (min(1.0, cov[z + 1] / 0.9) if z + 1 < len(cov) else 0)
    return h / sc


def ellipsis_labels():
    """Button and menu labels in the source that end in "..." or "\u2026"
    (SPM's rule: a label says what it does, no trailing dots)."""
    src = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "app", "src")
    pat = re.compile(r'(\.label\(|PopupMenuItem::new\(|RowMenuItem::|stop_label|Slot::Button\()[^\n]*?"([^"\n]*(?:\u2026|\.\.\.))"')
    found = []
    for root, _, files in os.walk(src):
        for f in files:
            if not f.endswith(".rs"):
                continue
            p = os.path.join(root, f)
            for n, line in enumerate(open(p, errors="replace"), 1):
                if line.lstrip().startswith("//"):
                    continue
                for m in pat.finditer(line):
                    found.append(f"{os.path.relpath(p, src)}:{n}: label \"{m.group(2)}\" ends in an ellipsis")
    return found


def main():
    out = sys.argv[1]
    groups = defaultdict(lambda: defaultdict(lambda: {"on": [], "off": []}))
    findings = []
    for f in sorted(os.listdir(out)):
        m = re.match(r"(.*)-(\d+)-x([\d.]+)-(light|dark)\.log$", f)
        # Setup's window has its own step list, not the manager's menu.
        if not m or "-SPM_DEBUG" in f or f.startswith("setup-"):
            continue
        png = os.path.join(out, f[:-4] + ".png")
        if not os.path.exists(png):
            continue
        width, sc, th = m.group(2), float(m.group(3)), m.group(4)
        frame = last_frame(os.path.join(out, f))
        labels = sorted((float(p[4]), p[1] == "menu-label-on", tuple(float(x) for x in p[2:6]))
                        for p in (l.split() for l in frame if l.startswith("LAYOUT menu-label")) if len(p) == 6)
        rows = sorted(tuple(float(x) for x in l.split()[2:6]) for l in frame if l.startswith("LAYOUT menu-row "))
        brand = [tuple(float(x) for x in l.split()[2:6]) for l in frame if l.startswith("LAYOUT menu-brand ")]
        name = f[:-4]
        # Row pitch: every row 40 px, the rows of one run 44 px apart (the
        # divider before Instances breaks the run).
        for r in rows:
            if abs((r[3] - r[2]) - ROW_H) > 0.6:
                findings.append(f"{name}: menu row {r[2]:.0f} is {r[3] - r[2]:.1f} px tall, not {ROW_H:.0f}")
        tops = [r[2] for r in rows]
        steps = [b - a for a, b in zip(tops, tops[1:])]
        bad = [s for s in steps if abs(s - PITCH) > 0.6 and s < PITCH + 10]
        if bad:
            findings.append(f"{name}: menu row pitch {bad} px, not {PITCH:.0f}")
        if brand and rows:
            gap = rows[0][2] - brand[-1][3]
            if abs(gap - NAV_TOP) > 0.6:
                findings.append(f"{name}: {gap:.1f} px from the brand row to the first row, not {NAV_TOP:.0f}")
        if not labels:
            continue
        im = Image.open(png).convert("RGB")
        for i, (_, on, box) in enumerate(labels):
            h = ink_height(im, box, sc)
            if h is not None:
                groups[(width, sc, th)][i]["on" if on else "off"].append((h, name))
    for key, rows in groups.items():
        for i, v in rows.items():
            if not v["on"] or not v["off"]:
                continue
            off = sorted(h for h, _ in v["off"])[len(v["off"]) // 2]
            sc = key[1]
            tol = TOL if abs(sc * 2 - round(sc * 2)) < 1e-6 else max(TOL, 1 / sc + 0.01)
            for h, name in v["on"]:
                if abs(h - off) > tol:
                    findings.append(f"{name}: selected label {i} is {h:.2f} px tall, unselected {off:.2f} px")
    findings += ellipsis_labels()
    for f in findings:
        print(f)
    compared = sum(1 for rows in groups.values() for v in rows.values() if v["on"] and v["off"])
    print(f"labels.py: {compared} selected labels compared, {len(findings)} findings")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
