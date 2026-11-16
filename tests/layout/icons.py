#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""The menu's icons stay put when it folds: every `LAYOUT menu-icon l r t b`
line of the last complete frame must have its centre at x = 32 (the folded
menu's middle), folded or not.

    icons.py <stderr log>
"""
import sys

ICON_X, TOL = 32.0, 0.6


def main():
    lines = open(sys.argv[1], errors="replace").read().splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if not marks:
        print("no layout lines")
        return 1
    start = marks[-2] if len(marks) > 1 else marks[-1]
    end = marks[-1] if len(marks) > 1 else len(lines)
    icons = [tuple(float(x) for x in l.split()[2:]) for l in lines[start + 1:end] if l.startswith("LAYOUT menu-icon ")]
    if not icons:
        print("no menu icons in the frame")
        return 1
    bad = [i for i in icons if abs((i[0] + i[1]) / 2 - ICON_X) > TOL]
    for l, r, t, b in bad:
        print(f"menu icon at y {t:.0f}: centre x {(l + r) / 2:.1f}, not {ICON_X:.0f}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
