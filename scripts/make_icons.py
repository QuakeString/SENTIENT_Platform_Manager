#!/usr/bin/env python3
"""Regenerate the Platform Manager icon set.

The source art (icons/icon.src.png) is the SENTIENT install mark on a flat
pure-cyan tile. Flat cyan renders as a harsh "single colour" — the intended
look is a gradient. This recolours *only* the cyan tile background to the brand
teal gradient (keeping the white mark, green arrow and disk), then emits every
PNG size Tauri needs plus a proper multi-size .ico (the old .ico only held
16/24 px, so Windows upscaled a tiny image — that was the "weird" taskbar icon).

Run with the venv Pillow:  /tmp/iconv/bin/python scripts/make_icons.py
"""
from PIL import Image
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ICONS = os.path.join(ROOT, "src-tauri", "icons")
SRC = os.path.join(ICONS, "icon.src.png")  # pristine copy of the original art

BG_CYAN = (0, 255, 255)          # the flat tile colour to replace
GRAD_TOP = (0x3a, 0xb8, 0xa4)    # brand teal, brighter at top
GRAD_BOT = (0x18, 0x5c, 0x51)    # deeper teal at the bottom
THRESH = 150.0                    # colour distance at which a pixel is "foreground"


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def recolour(src):
    """Replace the flat-cyan background with a vertical teal gradient, keeping
    the foreground art and the rounded-corner alpha intact."""
    src = src.convert("RGBA")
    w, h = src.size
    out = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    sp, op = src.load(), out.load()
    grad = [lerp(GRAD_TOP, GRAD_BOT, y / (h - 1)) for y in range(h)]
    for y in range(h):
        g = grad[y]
        for x in range(w):
            r, gg, b, a = sp[x, y]
            if a == 0:
                continue
            d = ((r - BG_CYAN[0]) ** 2 + (gg - BG_CYAN[1]) ** 2 + (b - BG_CYAN[2]) ** 2) ** 0.5
            fw = min(1.0, d / THRESH)  # 0 = background, 1 = foreground
            op[x, y] = (
                round(g[0] * (1 - fw) + r * fw),
                round(g[1] * (1 - fw) + gg * fw),
                round(g[2] * (1 - fw) + b * fw),
                a,
            )
    return out


def main():
    base = recolour(Image.open(SRC))  # 512×512 master
    base.save(os.path.join(ICONS, "icon.png"))

    sizes = {
        "32x32.png": 32,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
    }
    for name, s in sizes.items():
        base.resize((s, s), Image.LANCZOS).save(os.path.join(ICONS, name))

    # multi-size Windows .ico. Force BMP encoding — an all-PNG .ico renders as a
    # broken/blank icon in the Windows taskbar and NSIS installer; BMP small
    # sizes are what Explorer actually draws.
    base.save(os.path.join(ICONS, "icon.ico"),
              sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
              bitmap_format="bmp")

    # in-app brand logo
    base.resize((64, 64), Image.LANCZOS).save(os.path.join(ROOT, "src", "logo.png"))
    print("icons regenerated")


if __name__ == "__main__":
    main()
