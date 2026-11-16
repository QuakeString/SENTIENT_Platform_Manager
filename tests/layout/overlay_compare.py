#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""compare.py <log> <split>: the page's boxes (panels, buttons, top-bar
controls, monitors) in the last frame before the click must equal those in
the last frame with the menu open over the page; and the menu must be open."""
import sys
data = open(sys.argv[1], 'rb').read()
a, b = (int(x) for x in open(sys.argv[2]).read().split()[:2])
def last(seg):
    lines = seg.decode(errors='replace').splitlines()
    marks = [i for i, l in enumerate(lines) if l.startswith('LAYOUT-FRAME ')]
    s = marks[-2] if len(marks) > 1 else marks[-1]
    e = marks[-1] if len(marks) > 1 else len(lines)
    blk = lines[s + 1:e]
    keep = sorted(set(l for l in blk if l.split()[1:2] and l.split()[1] in ('panel', 'button', 'bar-ctrl', 'monitors', 'm-cpu', 'm-mem', 'm-disk')))
    ov = [l for l in blk if l.startswith('LAYOUT menu-overlay')]
    return keep, ov
k1, o1 = last(data[:a]); k2, o2 = last(data[a:b])
bad = 0
if o1: print('closed state already shows the overlay'); bad = 1
if not o2 or float(o2[-1].split()[2]) < 231: print('overlay not fully open:', o2[-1:] ); bad = 1
if k1 != k2:
    bad = 1
    for l in sorted(set(k1) ^ set(k2))[:12]: print(('closed ' if l in k1 else 'open   ') + l)
print(f'{len(k1)} page boxes compared, open width {o2[-1].split()[2] if o2 else "-"}')
sys.exit(bad)
