#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""One control height, one centre line per row, for one render
(SPM_DEBUG_LAYOUT=1 log):

    rows.py <stderr log>

The standard control height is the toolkit's input and button at their
default size (ui::control_h, 2 rem: STANDARD px on screen). Checks:

- every input (`LAYOUT field`, the settings field `field-input`, which
  reports the inside of its 1 px border), every settings control
  (`field-chips`, inside its border; `field-control`, the switch) and
  every segmented choice (`segmented`, inside its border) is STANDARD
  tall;
- controls side by side in a row (inputs, segmented choices and buttons,
  `LAYOUT button`: their y ranges overlap and at most GAP px lie between
  them) are one height and share one centre line, so a page header's
  range and Refresh, a form's field and its Choose…, a toolbar's search
  and its buttons line up top and bottom;
- a settings row: its name's first line (`setting-label`) and its current
  value (`current-cell`) are STANDARD tall and centred on the control's
  line in their row; in a wide row (a link, buttons, a choice) the first
  control beside the name is centred on the name's line.
Prints one line per finding; exits 1 if there is any.
"""
import sys

STANDARD = 28.0
TOL = 1.0
GAP = 48.0
CONTROLS = ("field", "field-input", "field-chips", "field-control", "segmented", "button")
FORM = ("field", "field-input", "field-chips", "field-control", "segmented")
# Bordered boxes report the inside of their 1 px border.
INSIDE = ("field-input", "field-chips", "segmented")


def frame(log):
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return None
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    body = lines[start + 1:end]
    # A dialog open: only what is drawn on it (after its `modal` probe, in
    # paint order); the page under it is covered.
    modal = [i for i, l in enumerate(body) if l.startswith("LAYOUT modal ")]
    if modal:
        body = body[modal[-1]:]
    out = {}
    for l in body:
        p = l.split()
        if len(p) == 6 and p[0] == "LAYOUT":
            r = tuple(float(x) for x in p[2:])
            if p[1] in INSIDE:  # the inside of its border: the box is 1 px more each way
                r = (r[0] - 1, r[1] + 1, r[2] - 1, r[3] + 1)
            if r[1] - r[0] <= 0 or r[3] - r[2] <= 0:
                continue  # not laid out (hidden)
            out.setdefault(p[1], set()).add(r)
    # A settings card's scrolling part clips what scrolls past it (under the
    # Apply bar): only what shows inside it counts (the bar holds buttons only).
    areas = out.get("page-area", set())
    bars = out.get("pending-bar", set())

    def inside(r, a):
        return a[0] - 0.5 <= r[0] and r[1] <= a[1] + 0.5 and a[2] - 0.5 <= r[2] and r[3] <= a[3] + 0.5

    for k in CONTROLS + ("setting-label", "current-cell"):
        if k in out:
            out[k] = {r for r in out[k] if not any(a[0] - 0.5 <= r[0] and r[1] <= a[1] + 0.5 and not inside(r, a) for a in areas) or (k == "button" and any(inside(r, b) for b in bars))}
    return {k: sorted(v) for k, v in out.items()}


def h(r):
    return r[3] - r[2]


def c(r):
    return (r[2] + r[3]) / 2


def main():
    log = sys.argv[1]
    f = frame(log)
    if f is None:
        return 0
    bad = []
    for kind in FORM:
        for r in f.get(kind, []):
            if abs(h(r) - STANDARD) > TOL:
                bad.append(f"{kind} at x {r[0]:.0f} y {r[2]:.0f}: {h(r):.0f} px tall, the standard is {STANDARD:.0f}")
    ctl = [(k, r) for k in CONTROLS for r in f.get(k, [])]
    seen = set()
    for i, (ka, a) in enumerate(ctl):
        for kb, b in ctl[i + 1:]:
            if a == b or (a[0] <= b[0] and b[1] <= a[1] and a[2] <= b[2] and b[3] <= a[3]) or (b[0] <= a[0] and a[1] <= b[1] and b[2] <= a[2] and a[3] <= b[3]):
                continue  # the same control, or one inside the other
            if min(a[3], b[3]) - max(a[2], b[2]) <= 0:
                continue  # not in one row
            gap = max(a[0], b[0]) - min(a[1], b[1])
            if gap > GAP or gap < 0:
                continue  # far apart, or one over the other (not side by side)
            if abs(h(a) - h(b)) > TOL or abs(c(a) - c(b)) > TOL:
                key = (round(a[0]), round(a[2]), round(b[0]), round(b[2]))
                if key in seen:
                    continue
                seen.add(key)
                bad.append(f"row at y {min(a[2], b[2]):.0f}: {ka} x {a[0]:.0f} ({h(a):.0f} px, centre {c(a):.1f}) beside {kb} x {b[0]:.0f} ({h(b):.0f} px, centre {c(b):.1f})")
    rowctl = [r for k in ("field-input", "field-chips", "field-control") for r in f.get(k, [])]
    for kind in ("setting-label", "current-cell"):
        for r in f.get(kind, []):
            mates = [x for x in rowctl if min(x[3], r[3]) - max(x[2], r[2]) > 0]
            if not mates:
                continue  # stacked: the name over its field
            m = mates[0]
            if kind == "current-cell" and abs(h(r) - STANDARD) > TOL:
                bad.append(f"current value at y {r[2]:.0f}: {h(r):.0f} px tall, the standard is {STANDARD:.0f}")
            if abs(r[2] - m[2]) > TOL:
                bad.append(f"{kind} at y {r[2]:.0f}: its first line is not on its control's line (control top {m[2]:.0f}, centre {c(m):.1f})")
            elif abs(h(r) - STANDARD) <= TOL and abs(c(r) - c(m)) > TOL:
                bad.append(f"{kind} at y {r[2]:.0f}: centre {c(r):.1f}, its control's {c(m):.1f}")
    # A wide settings row (a link, buttons, a choice across the field and
    # current columns): its control on the name's first line.
    others = [r for k in ("button", "field", "segmented") for r in f.get(k, [])]
    for r in f.get("setting-label", []):
        if abs(h(r) - STANDARD) > TOL or any(min(x[3], r[3]) - max(x[2], r[2]) > 0 for x in rowctl):
            continue
        mates = [x for x in others if x[0] >= r[1] - 0.5 and min(x[3], r[3]) - max(x[2], r[2]) > 0]
        if mates:
            m = min(mates, key=lambda x: x[0])
            if abs(c(m) - c(r)) > TOL:
                bad.append(f"setting-label at y {r[2]:.0f}: centre {c(r):.1f}, the control beside it (x {m[0]:.0f}, {h(m):.0f} px) {c(m):.1f}")
    for b in bad:
        print(f"{log.split('/')[-1]}: {b}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
