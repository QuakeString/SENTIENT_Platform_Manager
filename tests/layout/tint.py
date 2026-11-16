#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""The instance tint of the top bar is as faint in dark as in light.

    tint.py <out dir>

For every run.sh render (`<name>-<width>-x<scale>-<theme>.log/.png`, with
`LAYOUT top-bar l r t b` and `LAYOUT menu-edge x`), the bar's own colour (in
its right padding, mid-height) and the page's (just under the bar) are
sampled and their CIELAB colour difference taken. The same render in light
and in dark must differ by about the same amount: |dE dark - dE light| at
most 25 % of the light value (3 at least).

Instance tiles (`LAYOUT inst-card`, `inst-card-head` and the plain card
colour `inst-card-surface r g b`): the body's colour difference from the
plain card is at most half the header's, in both themes, so
a tile reads as a tinted header over a nearly plain card.
Prints findings, exits 1 on any.
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


def frame(lines):
    marks = [i for i, l in enumerate(lines) if l.startswith("LAYOUT-FRAME ")]
    if len(marks) < 2:
        return lines
    return lines[marks[-2] + 1:marks[-1]]


def tile_findings(lines, png, name, findings):
    """Per tile: dE(body, card) <= 0.5 * dE(header, card)."""
    fr = frame(lines)
    surf = [l.split()[2:5] for l in lines if l.startswith("LAYOUT inst-card-surface ")]
    if not surf:
        return 0
    plain = tuple(int(float(x)) for x in surf[-1])
    cards = [[float(x) for x in l.split()[2:6]] for l in fr if l.startswith("LAYOUT inst-card ")]
    heads = [[float(x) for x in l.split()[2:6]] for l in fr if l.startswith("LAYOUT inst-card-head ")]
    if not cards or not heads:
        return 0
    sc = float(re.search(r"-x([\d.]+)-(light|dark)$", name).group(1))
    im = Image.open(png).convert("RGB")
    n = 0
    for c in cards:
        hd = [h for h in heads if abs(h[0] - c[0]) < 4 and abs(h[2] - c[2]) < 4]
        if not hd:
            continue
        l, r, t, b = c
        h = hd[0]
        if t * sc < 0 or b * sc > im.height or r * sc > im.width:
            continue
        px_ = lambda x, y: im.getpixel((int(x * sc), int(y * sc)))
        head = px_(r - 5, h[2] + 4)
        body = px_(l + 5, h[3] + 5)
        dh, db = de(head, plain), de(body, plain)
        n += 1
        if dh > 1 and db > 0.5 * dh + 0.5:
            findings.append(f"{name}: tile at {l:.0f},{t:.0f}: body dE {db:.1f}, header dE {dh:.1f} (body over half)")
    return n


def over(top, a, under):
    return tuple(t * a + u * (1 - a) for t, u in zip(top, under))


def shimmer_de(lines):
    """The skeleton band's colour difference from its block (the logged
    card, block and band colours composited), or None."""
    hits = [l.split()[2:] for l in lines if l.startswith("LAYOUT skeleton-colors ")]
    if not hits:
        return None
    v = [float(x) for x in hits[-1]]
    card = tuple(v[0:3])
    block = over(tuple(v[4:7]), v[7], card)
    band = over(tuple(v[8:11]), v[11], block)
    return de(block, band)


def main():
    out = sys.argv[1]
    seen = {}
    findings, compared, tiles = [], 0, 0
    shimmer = {}
    for f in sorted(os.listdir(out)):
        m = re.match(r"(.*-\d+-x[\d.]+)-(light|dark)\.log$", f)
        if not m or not os.path.exists(os.path.join(out, f[:-4] + ".png")):
            continue
        lines = open(os.path.join(out, f), errors="replace").read().splitlines()
        bar, edge = last(lines, "LAYOUT top-bar "), last(lines, "LAYOUT menu-edge ")
        tiles += tile_findings(lines, os.path.join(out, f[:-4] + ".png"), f[:-4], findings)
        sh = shimmer_de(lines)
        if sh is not None:
            shimmer.setdefault(m.group(1), {})[m.group(2)] = sh
        if not bar or not edge:
            continue
        sc = float(re.search(r"-x([\d.]+)$", m.group(1)).group(1))
        im = Image.open(os.path.join(out, f[:-4] + ".png")).convert("RGB")
        l, r, t, b = bar
        bar_px = im.getpixel((int((r - 5) * sc), int((t + b) / 2 * sc)))
        page_px = im.getpixel((int((edge[0] + 6) * sc), int((b + 6) * sc)))
        seen.setdefault(m.group(1), {})[m.group(2)] = de(bar_px, page_px)
    for name, v in sorted(seen.items()):
        if "light" in v and "dark" in v:
            compared += 1
            lt, dk = v["light"], v["dark"]
            if abs(dk - lt) > max(3.0, 0.25 * lt):
                findings.append(f"{name}: bar tint dE {dk:.1f} in dark, {lt:.1f} in light")
    # The loading shimmer's band at least as visible in dark as in light.
    for name, v in sorted(shimmer.items()):
        if "light" in v and "dark" in v and v["dark"] + 0.5 < v["light"]:
            findings.append(f"{name}: skeleton shimmer dE {v['dark']:.1f} in dark, {v['light']:.1f} in light")
    for x in findings:
        print(x)
    print(f"tint.py: {compared} light/dark pairs compared, {tiles} instance tiles, {len(findings)} findings")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
