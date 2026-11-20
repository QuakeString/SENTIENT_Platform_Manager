#!/usr/bin/env python3
# Copyright (c) 2016-2026 The SENTIENT Authors
#
# The Microsoft Store package's logos (packaging/msix/Assets), made from the
# Platform Manager's app icon (app/assets/app-icon: its own small sizes,
# else 512.png scaled). The files carry
# MRT qualifiers (scale-*, targetsize-*, altform-unplated): the manifest
# names Assets\Square44x44Logo.png and the like, and the package's
# resources.pri (scripts/package-msix.ps1, makepri.exe) maps each name to
# the file that fits the screen.
#
# Run again after the app icon changes: python3 packaging/msix/make-assets.py
# Needs Pillow (pip install pillow).

import os
import sys

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCE = os.path.join(HERE, "..", "..", "app", "assets", "app-icon", "512.png")
OUT = os.path.join(HERE, "Assets")


def icon(size):
    # The hand-drawn small sizes of the app icon where there is one.
    exact = os.path.join(os.path.dirname(SOURCE), "%d.png" % size)
    if os.path.isfile(exact):
        img = Image.open(exact).convert("RGBA")
        if img.size == (size, size):
            return img
    return Image.open(SOURCE).convert("RGBA").resize((size, size), Image.LANCZOS)


def canvas(w, h, icon_px):
    """The icon centred on a transparent w x h canvas, icon_px tall."""
    c = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    i = icon(icon_px)
    c.alpha_composite(i, ((w - icon_px) // 2, (h - icon_px) // 2))
    return c


def save(img, name):
    img.save(os.path.join(OUT, name), optimize=True)


def main():
    if not os.path.isfile(SOURCE):
        sys.exit("missing " + SOURCE)
    os.makedirs(OUT, exist_ok=True)
    for old in os.listdir(OUT):
        if old.endswith(".png"):
            os.remove(os.path.join(OUT, old))
    # App list, taskbar, Start: the icon edge to edge (its own rounded
    # square), at each scale and each fixed size; unplated for the taskbar.
    for scale in (100, 125, 150, 200, 400):
        save(icon(round(44 * scale / 100)), "Square44x44Logo.scale-%d.png" % scale)
    for size in (16, 24, 32, 48, 256):
        save(icon(size), "Square44x44Logo.targetsize-%d.png" % size)
        save(icon(size), "Square44x44Logo.targetsize-%d_altform-unplated.png" % size)
    # Tiles: the icon with a margin, as Start draws its own plate.
    for scale in (100, 200):
        f = scale / 100
        save(canvas(round(150 * f), round(150 * f), round(96 * f)), "Square150x150Logo.scale-%d.png" % scale)
        save(canvas(round(310 * f), round(150 * f), round(96 * f)), "Wide310x150Logo.scale-%d.png" % scale)
        save(icon(round(50 * f)), "StoreLogo.scale-%d.png" % scale)
    print("wrote", len([n for n in os.listdir(OUT) if n.endswith(".png")]), "files to", OUT)


if __name__ == "__main__":
    main()
