#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Splits the texts still missing in a language into chunk files to translate.

    split.py <code> [size]   -> app/i18n/work/<code>/in-NN.json ({english: ""})
"""
import json, os, sys
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../.."))
code = sys.argv[1]; size = int(sys.argv[2]) if len(sys.argv) > 2 else 400
en = json.load(open(f"{ROOT}/app/i18n/en.json", encoding="utf-8"))
path = f"{ROOT}/app/i18n/{code}.json"
have = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
todo = [k for k in en if not have.get(k)]
work = f"{ROOT}/app/i18n/work/{code}"; os.makedirs(work, exist_ok=True)
for i in range(0, len(todo), size):
    json.dump({k: "" for k in todo[i:i + size]}, open(f"{work}/in-{i // size:02d}.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(f"{code}: {len(todo)} texts in {(len(todo) + size - 1) // size} chunks under {work}")
