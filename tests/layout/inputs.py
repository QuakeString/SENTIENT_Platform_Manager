#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""One control height in the source (rows.py measures it on screen):

    tests/layout/inputs.py

- every text input is the toolkit's at its default size: no `.small()`,
  `.xsmall()`, `.large()`, `.with_size(..)` or `.h(..)` on an
  `Input::new(..)` chain;
- no button set to a height other than the standard (28 px, ui::control_h),
  except the bars' own controls (crate::theme::BAR_CTRL_H: the top bar and
  Setup's footer, whose `h` is BAR_CTRL_H, one height of their own
  across each bar);
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
                    hm = re.match(r"^\.h\(px\(([0-9.]+)\)\)$", link)
                    if hm and abs(float(hm.group(1)) - STANDARD) > 0.01:
                        bad.append(f"{rel}:{line_of(text, m.start())}: button {hm.group(1)} px tall, the standard is {STANDARD:.0f}")
                    elif link.startswith(".h(") and not hm and "BAR_CTRL_H" not in link and not (link == ".h(h)" and rel == os.path.join("installer", "mod.rs")):
                        bad.append(f"{rel}:{line_of(text, m.start())}: button with a height of its own: {link}")
            for m in re.finditer(r"\b(?:seg_track|input_box)\(", text):
                for link in chain(text, m.end() - 1):
                    if re.match(r"^\.(h|min_h|max_h|size)\(", link):
                        bad.append(f"{rel}:{line_of(text, m.start())}: control box with a height of its own: {link}")
    for b in bad:
        print(b)
    if not bad:
        print("inputs.py: every input, segmented choice and button is the standard height")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
