#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""The instance tint of the top bar is as faint in dark as in light.

    tint.py <out dir>

For every run.sh render (`<name>-<width>-x<scale>-<theme>.log/.png`, with
`LAYOUT top-bar l r t b` and `LAYOUT menu-edge x`), the bar's own colour (in
its right padding, mid-height) and the page's (just under the bar) are
sampled and their CIELAB colour difference taken. The same render in light
and in dark must differ by about the same amount: |dE dark - dE light| at
most 25 % of the light value (3 at least). Prints findings, exits 1 on any.
"""
import os
import re
import sys
from PIL import Image


def lab(c):
    def f(u):
        u /= 255
        return ((u + 0.055) / 1.055) ** 2.4 if u > 0.04045 else u / 12.92
    r, g, b = map(f, c[:3])
    X = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047
    Y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    Z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883
    h = lambda t: t ** (1 / 3) if t > 0.008856 else 7.787 * t + 16 / 116
    fx, fy, fz = h(X), h(Y), h(Z)
    return (116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz))


def de(a, b):
    A, B = lab(a), lab(b)
    return sum((x - y) ** 2 for x, y in zip(A, B)) ** 0.5


def last(lines, key):
    hits = [l.split() for l in lines if l.startswith(key)]
    return [float(x) for x in hits[-1][2:]] if hits else None


def main():
    out = sys.argv[1]
    seen = {}
    for f in sorted(os.listdir(out)):
        m = re.match(r"(.*-\d+-x[\d.]+)-(light|dark)\.log$", f)
        if not m or not os.path.exists(os.path.join(out, f[:-4] + ".png")):
            continue
        lines = open(os.path.join(out, f), errors="replace").read().splitlines()
        bar, edge = last(lines, "LAYOUT top-bar "), last(lines, "LAYOUT menu-edge ")
        if not bar or not edge:
            continue
        sc = float(re.search(r"-x([\d.]+)$", m.group(1)).group(1))
        im = Image.open(os.path.join(out, f[:-4] + ".png")).convert("RGB")
        l, r, t, b = bar
        bar_px = im.getpixel((int((r - 5) * sc), int((t + b) / 2 * sc)))
        page_px = im.getpixel((int((edge[0] + 6) * sc), int((b + 6) * sc)))
        seen.setdefault(m.group(1), {})[m.group(2)] = de(bar_px, page_px)
    findings, compared = [], 0
    for name, v in sorted(seen.items()):
        if "light" in v and "dark" in v:
            compared += 1
            lt, dk = v["light"], v["dark"]
            if abs(dk - lt) > max(3.0, 0.25 * lt):
                findings.append(f"{name}: bar tint dE {dk:.1f} in dark, {lt:.1f} in light")
    for x in findings:
        print(x)
    print(f"tint.py: {compared} light/dark pairs compared, {len(findings)} findings")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
