#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""One button size in every render (inputs.py checks the same in the source):

    tests/layout/buttons.py <log> [<log> …]
    tests/layout/buttons.py <out dir> [<out dir> …]

Every button reports itself in the layout test (`LAYOUT btn <label|icon>
l r t b <font px> <id>`: the toolkit's Button with SPM_DEBUG_LAYOUT, and
crate::ui::solid_button; a `link` line, a .link()/.text() button drawn as
text, is not held to the box). In the last complete frame of every log:

- a labelled button is crate::theme::BUTTON_H (28 px) tall, at least
  BUTTON_MIN_W (72 px) wide, its label one font size;
- an icon-only button is the ICON_BUTTON square (28 x 28 px);
- across every log given, one height and one label font size (a render
  at another scale or theme shows the same logical sizes).

The toolkit's buttons inside other controls (an input's clear button, a
select's caret) are not app buttons; INNER names them by id.
Prints one line per finding; exits 1 if there is any.
"""
import glob
import os
import sys

BUTTON_H = 28.0
BUTTON_MIN_W = 72.0
TOL = 0.6
# Toolkit-internal buttons, drawn inside a control of their own.
INNER = ("clean", "clear", "toggle-mask", "caret", "spin-up", "spin-down")


def last_frame(lines):
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if len(marks) < 2:
        return lines
    return lines[marks[-2] + 1:marks[-1]]


def buttons(log):
    out = {}
    for l in last_frame(open(log, errors="replace").read().splitlines()):
        if not l.startswith("LAYOUT btn "):
            continue
        p = l.split(" ", 8)
        if len(p) < 8:
            continue
        kind, (x0, x1, y0, y1, fs) = p[2], (float(v) for v in p[3:8])
        bid = p[8] if len(p) > 8 else ""
        # A link or text button (the toolkit's .link()/.text()) is text.
        if kind == "link" or any(n in bid for n in INNER):
            continue
        out[(kind, round(x0, 1), round(y0, 1), bid)] = (x1 - x0, y1 - y0, fs)
    return out


def main(args):
    logs = []
    for a in args:
        logs += sorted(glob.glob(os.path.join(a, "*.log"))) if os.path.isdir(a) else [a]
    bad, heights, fonts, seen = [], set(), set(), 0
    for log in logs:
        name = os.path.basename(log)
        for (kind, x, y, bid), (w, h, fs) in buttons(log).items():
            seen += 1
            heights.add(round(h, 1))
            if kind == "label":
                fonts.add(round(fs, 2))
                if abs(h - BUTTON_H) > TOL:
                    bad.append(f"{name}: button {bid} at ({x:.0f}, {y:.0f}) is {h:.2f} px tall, not {BUTTON_H:.0f}")
                if w < BUTTON_MIN_W - TOL:
                    bad.append(f"{name}: button {bid} at ({x:.0f}, {y:.0f}) is {w:.2f} px wide, under {BUTTON_MIN_W:.0f}")
            elif abs(h - BUTTON_H) > TOL or abs(w - BUTTON_H) > TOL:
                bad.append(f"{name}: icon button {bid} at ({x:.0f}, {y:.0f}) is {w:.2f} x {h:.2f} px, not {BUTTON_H:.0f} x {BUTTON_H:.0f}")
    if len(fonts) > 1:
        bad.append(f"button labels in {len(fonts)} font sizes: {sorted(fonts)}")
    if logs and not seen:
        bad.append("no `LAYOUT btn` lines: was SPM_DEBUG_LAYOUT set on a debug build?")
    for b in bad:
        print(b)
    # Silent on one render that passes: run.sh's SKIP_DONE takes an empty
    # .result as a pass.
    if not bad and len(logs) > 1:
        print(f"buttons.py: {seen} buttons in {len(logs)} renders, one size ({sorted(heights)} px, label {sorted(fonts)} px)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
