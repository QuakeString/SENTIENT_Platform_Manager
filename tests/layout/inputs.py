#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""One control height in the source (rows.py measures it on screen):

    tests/layout/inputs.py

- every text input is the toolkit's at its default size: no `.small()`,
  `.xsmall()`, `.large()`, `.with_size(..)` or `.h(..)` on an
  `Input::new(..)` chain;
- one button size, no exceptions (crate::theme::BUTTON_H, BUTTON_PAD_X,
  BUTTON_MIN_W, BUTTON_RADIUS, set by crate::ui::button): no size, height,
  width, padding, corner or font-size link on a `button(..)` chain, and no
  `Button::new(..)` outside crate::ui (buttons.py measures every button in
  every render);
- a segmented choice and an input box keep the standard height (no `.h(..)`
  on a `seg_track(..)` or `input_box()` chain).
Prints one line per finding; exits 1 if there is any.
"""
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "app", "src")
STANDARD = 28.0
SIZE = re.compile(r"^\.(small|xsmall|large|with_size|h|size)\(")
BUTTON_SIZE = re.compile(r"^\.(small|xsmall|large|with_size|compact|label_box|h|w|size|min_w|min_h|max_w|max_h|px|py|p|pl|pr|pt|pb|rounded|text_size|text_xs|text_sm|text_base|text_lg|line_height|icon_size|(?:h|w|size|min_w|px|py|p|pl|pr|rounded)_[0-9a-z]+)\(")


def chain(text, i):
    """The method chain that follows the call whose '(' is at i: the call's
    arguments skipped, then `.name(args)` links, across lines."""
    out = []
    depth = 0
    j = i
    while j < len(text):
        ch = text[j]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                j += 1
                break
        j += 1
    while True:
        k = j
        while k < len(text) and text[k] in " \t\r\n":
            k += 1
        if k >= len(text) or text[k] != ".":
            return out
        m = re.match(r"\.([A-Za-z_][A-Za-z0-9_]*)", text[k:])
        if not m:
            return out
        p = k + m.end()
        if p < len(text) and text[p] == "(":
            q = p
            d = 0
            while q < len(text):
                if text[q] == "(":
                    d += 1
                elif text[q] == ")":
                    d -= 1
                    if d == 0:
                        break
                q += 1
            out.append(text[k:q + 1])
            j = q + 1
        else:
            out.append(text[k:p])
            j = p


def line_of(text, i):
    return text.count("\n", 0, i) + 1


def main():
    bad = []
    for dirpath, _, files in os.walk(ROOT):
        for name in files:
            if not name.endswith(".rs"):
                continue
            path = os.path.join(dirpath, name)
            text = open(path, encoding="utf-8").read()
            rel = os.path.relpath(path, ROOT)
            for m in re.finditer(r"\bInput::new\(", text):
                for link in chain(text, m.end() - 1):
                    if SIZE.match(link):
                        bad.append(f"{rel}:{line_of(text, m.start())}: input sized off the standard: {link}")
            for m in re.finditer(r"\b(?:ui::)?button\(", text):
                for link in chain(text, m.end() - 1):
                    if BUTTON_SIZE.match(link):
                        bad.append(f"{rel}:{line_of(text, m.start())}: button sized off the one button size: {link}")
            if rel != "ui.rs":
                for m in re.finditer(r"\bButton::new\(", text):
                    bad.append(f"{rel}:{line_of(text, m.start())}: a button not made by crate::ui::button")
            for m in re.finditer(r"\b(?:seg_track|input_box)\(", text):
                for link in chain(text, m.end() - 1):
                    if re.match(r"^\.(h|min_h|max_h|size)\(", link):
                        bad.append(f"{rel}:{line_of(text, m.start())}: control box with a height of its own: {link}")
    for b in bad:
        print(b)
    if not bad:
        print("inputs.py: every input and segmented choice is the standard height, every button the one button size")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
