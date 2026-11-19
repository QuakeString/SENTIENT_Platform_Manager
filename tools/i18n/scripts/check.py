#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Merges a language's translated chunks into app/i18n/<code>.json and checks it:
every placeholder ({0}, {name}) of the English is there, no new ones, line
breaks kept, nothing empty. Bad texts are left out (they show in English) and
listed.

    check.py <code>          merge work/<code>/out-*.json, check, write
    check.py <code> --only   check app/i18n/<code>.json as it is
"""
import glob, json, os, re, sys
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../.."))
code = sys.argv[1]
en = json.load(open(f"{ROOT}/app/i18n/en.json", encoding="utf-8"))
path = f"{ROOT}/app/i18n/{code}.json"
tr = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
if "--only" not in sys.argv:
    for f in sorted(glob.glob(f"{ROOT}/app/i18n/work/{code}/out-*.json")):
        try:
            tr.update({k: v for k, v in json.load(open(f, encoding="utf-8")).items() if isinstance(v, str)})
        except Exception as e:
            print(f"{f}: not valid JSON: {e}")
ph = lambda s: sorted(re.findall(r"\{[A-Za-z0-9_]*\}", s.replace("{{", "").replace("}}", "")))
bad, good = [], {}
for k in en:
    v = tr.get(k)
    if not v:
        continue
    if ph(v) != ph(k):
        bad.append((k, v, "placeholders")); continue
    if k.count("\n") and not v.count("\n"):
        bad.append((k, v, "line breaks")); continue
    # Texts are joined to others: keep the English's leading and trailing spaces.
    v = k[:len(k) - len(k.lstrip(" "))] + v.strip(" ") + k[len(k.rstrip(" ")):]
    good[k] = v
json.dump(good, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=1, sort_keys=False)
missing = len(en) - len(good)
print(f"{code}: {len(good)}/{len(en)} texts, {missing} missing, {len(bad)} rejected")
if missing:
    os.makedirs(f"{ROOT}/app/i18n/work/{code}", exist_ok=True)
    todo = f"{ROOT}/app/i18n/work/{code}/missing.json"
    json.dump({k: "" for k in en if k not in good}, open(todo, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    print(f"  the missing texts are listed in {todo}: translate them into an out-NN.json (e.g. out-99.json)")
for k, v, why in bad[:20]:
    print(f"  {why}: {k[:60]!r} -> {v[:60]!r}")
sys.exit(0 if missing == 0 else 1)
