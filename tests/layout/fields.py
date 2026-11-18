#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Settings forms for one render (SPM_DEBUG_LAYOUT=1 log):

    fields.py <stderr log> [debug env, e.g. SPM_DEBUG_SETTINGS_PENDING=short]

- every field (`LAYOUT field-input`) is one width and, on one page, ends
  at one x: the fields' right edges line up down the page;
- every unit slot (`LAYOUT field-slot`) is one width and sits at its
  field's right edge, inside its border;
- the Apply bar (`LAYOUT pending-bar`) lies inside the settings card
  (`LAYOUT settings-card`) along its bottom edge, and the card's
  scrolling part (`LAYOUT page-area`) ends where the bar starts;
- with SPM_DEBUG_SETTINGS_PENDING set (not "leave", whose dialog covers
  the page) the bar is there.
Prints one line per finding; exits 1 if there is any.
"""
import sys


def frame(log):
    lines = open(log, errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        return None
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    out = {}
    for l in lines[start + 1:end]:
        p = l.split()
        if len(p) == 6 and p[0] == "LAYOUT":
            out.setdefault(p[1], set()).add(tuple(float(x) for x in p[2:]))
    return {k: sorted(v) for k, v in out.items()}


def main():
    log = sys.argv[1]
    dbg = sys.argv[2] if len(sys.argv) > 2 else ""
    f = frame(log)
    if f is None:
        return 0
    bad = []
    card = f.get("settings-card", [])
    # Fields scrolled out of the card are still laid out: only those within
    # the card's visible height count for the x checks (all do anyway).
    fields = f.get("field-input", [])
    if fields:
        widths = sorted({round(r - l) for (l, r, t, b) in fields})
        if len(widths) > 1:
            bad.append(f"fields: widths {widths} differ")
        rights = sorted({round(r) for (l, r, t, b) in fields})
        if max(rights) - min(rights) > 1:
            bad.append(f"fields: right edges {rights} do not line up")
    slots = f.get("field-slot", [])
    if slots:
        sw = sorted({round(r - l) for (l, r, t, b) in slots})
        if len(sw) > 1:
            bad.append(f"unit slots: widths {sw} differ")
        for (l, r, t, b) in slots:
            home = [x for x in fields if x[2] - 0.5 <= t and b <= x[3] + 0.5 and x[0] <= l and r <= x[1]]
            if not home:
                bad.append(f"unit slot {l:.0f}-{r:.0f} y {t:.0f}: not inside a field")
            elif abs(home[0][1] - r) > 1.5:
                bad.append(f"unit slot {l:.0f}-{r:.0f} y {t:.0f}: not at its field's right edge ({home[0][1]:.0f})")
    bars = f.get("pending-bar", [])
    # The gateway card: no bar for a gateway not changed from here.
    pending = (dbg.startswith("SPM_DEBUG_SETTINGS_PENDING=") and not dbg.endswith("=leave")) or ("SPM_DEBUG_GATEWAY_PENDING=1" in dbg and "=compose" not in dbg and "native-unreadable" not in dbg)
    if pending and not bars:
        bad.append("Apply bar: not shown with changes pending")
    for (l, r, t, b) in bars:
        home = [c for c in card if c[0] - 0.5 <= l and r <= c[1] + 0.5 and c[2] <= t and b <= c[3] + 0.5]
        if not home:
            bad.append(f"Apply bar {l:.0f}-{r:.0f} x {t:.0f}-{b:.0f}: not inside the settings card")
            continue
        c = home[0]
        if abs(c[3] - b) > 1.5:
            bad.append(f"Apply bar: ends at y {b:.0f}, the card at {c[3]:.0f}")
        if abs(l - c[0]) > 1.5 or abs(r - c[1]) > 1.5:
            bad.append(f"Apply bar: {l:.0f}-{r:.0f} does not span the card {c[0]:.0f}-{c[1]:.0f}")
        areas = [a for a in f.get("page-area", []) if c[0] - 0.5 <= a[0] and a[1] <= c[1] + 0.5]
        # The bar's probe sits inside its 1 px top line, which snaps to a
        # whole device pixel: up to 2 px below the scrolling part at 2x.
        if areas and not (-1.5 <= t - areas[0][3] <= 2.5):
            bad.append(f"Apply bar: starts at y {t:.0f}, the card's scrolling part ends at {areas[0][3]:.0f}")
    for b in bad:
        print(f"{log.split('/')[-1]}: {b}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
