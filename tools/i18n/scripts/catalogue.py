#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Writes app/i18n/en.json, the list of every text to translate:

  - the app's t!/tf!/n!/nf! texts and the settings catalogue (spm-i18n-tool collect),
  - the engines' own words (crates/): job steps, messages and labels the
    Platform Manager shows as they come (crate::i18n::dynamic).

    catalogue.py            (from the repository root)
"""
import json
import os
import re
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../.."))
TOOL = os.path.join(ROOT, "tools/i18n/target/debug/spm-i18n-tool")
OUT = os.path.join(ROOT, "app/i18n/en.json")
TMP = os.environ.get("TMPDIR", "/tmp")


def normalize(text):
    """tf!'s key: {} and {:spec} as {0}, {1}; {name:spec} as {name}."""
    out, n, i = [], 0, 0
    while i < len(text):
        c = text[i]
        if text.startswith("{{", i) or text.startswith("}}", i):
            out.append(text[i:i + 2]); i += 2; continue
        if c == "{":
            j = text.index("}", i)
            arg = text[i + 1:j].split(":")[0].strip()
            if not arg:
                arg = str(n); n += 1
            out.append("{" + arg + "}"); i = j + 1; continue
        out.append(c); i += 1
    return "".join(out)


def main():
    subprocess.run([TOOL, "collect", os.path.join(ROOT, "app/src"), OUT], check=True)
    texts = json.load(open(OUT, encoding="utf-8"))
    scan, decided = os.path.join(TMP, "spm-crates-scan.tsv"), os.path.join(TMP, "spm-crates-decided.tsv")
    subprocess.run([TOOL, "scan", os.path.join(ROOT, "crates"), scan], check=True)
    subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "decide.py"), scan, decided], check=True)
    added = 0
    for line in open(decided, encoding="utf-8").read().splitlines()[1:]:
        d, _, _, _, _, kind, text = line.split("\t", 6)
        # integration tests (crates/*/tests/) are not shown to anyone
        if d != "yes" or "/tests/" in line.split("\t", 6)[2]:
            continue
        text = text.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r").replace("\\\\", "\\")
        key = normalize(text) if kind == "tf" else text
        if key not in texts:
            texts[key] = ""
            added += 1
    json.dump(texts, open(OUT, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    open(OUT, "a").write("\n")
    words = sum(len(k.split()) for k in texts)
    print(f"{len(texts)} texts ({added} from the engines), {words} words", file=sys.stderr)


main()
