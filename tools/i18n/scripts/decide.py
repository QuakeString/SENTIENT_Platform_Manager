#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Turns scan rows marked `maybe` into `yes` or `no` with the rules below,
and `const` rows into `const` (wrapped in n!, translated where used).

    decide.py scan.tsv decided.tsv
"""
import re
import sys

# Texts that are names or file contents, never shown as words.
SKIP_TEXTS = {"SENTIENT backup", "settings_catalog.json is valid", "SENTIENT Platform Manager restore; secrets key file: ",
              "SENTIENT Platform Manager (SPM)", "MesloLGS NF", "SENTIENT Light", "SENTIENT Dark", "#6b4fa0", "closed by remote host",
              "The window closed and there is no tray: quitting", "SENTIENT delayed start", "SENTIENT Platform", "True", "False", "Infinity", "Seat:*", "Autologin", "Accept", "Bypass"}
SKIP_FILES = {
    "i18n.rs", "agent.rs", "rootjob.rs", "elevate.rs", "terminal/prompt.rs", "remote/serve.rs",
    "remote/winshell.rs", "popup_gate.rs", "timing.rs", "log_level.rs", "remote/conn.rs",
}
BRANDS = {"SENTIENT", "SENTIENT Platform Manager", "INVENIA SYSTEMS", "SENTIENT Setup", "PLATFORM MANAGER", "Platform Manager"}
PROTOCOL_FIRST = {"LOG:", "HINT:", "FATAL:", "SET", "RENAME", "EXCHANGE", "INVE", "PART", "HKLM", "LAYOUT", "SELECT", "ALTER", "CREATE", "DROP", "GRANT", "REVOKE"}


def plain(t):
    return re.sub(r"\{[^{}]*\}", "", t)


def verdict(row):
    d, reason, f, text = row[0], row[1], row[2], row[6]
    if f in SKIP_FILES:
        return "no", "file skipped"
    if d not in ("yes", "maybe", "const"):
        return d, reason
    raw = text.replace("\\n", "\n").replace("\\t", "\t")
    p = plain(raw).strip()
    first = p.split(" ")[0] if p else ""
    if "\t" in raw or "\\SOFTWARE" in raw or "SOFTWARE\\" in raw:
        return "no", "tab-or-registry"
    if raw in SKIP_TEXTS:
        return "no", "listed"
    if raw.strip() in BRANDS:
        return "no", "brand"
    if first in PROTOCOL_FIRST:
        return "no", "protocol"
    if re.match(r"^[a-z0-9]+-[a-z0-9-]+\b", p):
        return "no", "command"
    if re.match(r"^[A-Z]{2,} [A-Z]{2,}( |$)", p) and not p.startswith("SENTIENT"):
        return "no", "caps"
    words = re.findall(r"[^\W\d_]{2,}", p)
    fmt = reason.startswith("fmt ")
    if d == "maybe" and p[:1].islower() and len(words) < 3 and not (fmt and words and "{" in raw):
        return "no", "lowercase fragment"
    if fmt and re.match(r"^(create|write|read|delete|copy|start|note:|action|ok|failed|go|user|the plan|the fix|unexpected answer|rgb\()\b", p.strip()):
        return "no", "internal message"
    if "=" in p and not " = " in p:
        return "no", "key=value"
    if f == "headless.rs":
        return "no", "command line"
    if d == "maybe" and not words:
        return "no", "no words"
    if d == "const":
        return "const", reason
    return "yes", reason


rows = [l.rstrip("\n").split("\t", 6) for l in open(sys.argv[1], encoding="utf-8")]
out = [rows[0]]
counts = {}
for r in rows[1:]:
    v, why = verdict(r)
    counts[v] = counts.get(v, 0) + 1
    r[0] = v
    if v != r[0]:
        r[1] = why
    r[1] = why if why else r[1]
    out.append(r)
with open(sys.argv[2], "w", encoding="utf-8") as fh:
    for r in out:
        fh.write("\t".join(r) + "\n")
print(counts, file=sys.stderr)
