#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""The Platform Manager's third-party licences (About > Third-party licences).

Writes app/assets/third_party_licences.json from `cargo metadata`: every
crate the app is built from (normal dependencies, default features, on the
platforms it is released for), its version and its licence, plus the bundled assets that
are not crates (icons, logos, the terminal font).

Checked in rather than made in build.rs: running cargo from inside a build
waits on the package-cache lock the outer cargo holds. The app's test
`third_party::tests::list_matches_cargo_lock` fails when Cargo.lock no
longer matches the file; run this script again then:

    scripts/third-party-licences.py            # write the file
    scripts/third-party-licences.py --check    # exit 1 when out of date
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "app")
OUT = os.path.join(APP, "assets", "third_party_licences.json")
VENDOR = os.path.join(APP, "vendor") + os.sep

# Bundled files that are not crates (their licence files sit beside them).
ASSETS = [
    {"name": "Lucide icons", "license": "ISC", "repository": "https://github.com/lucide-icons/lucide", "note": "assets/icons/LICENSE-LUCIDE"},
    {"name": "Simple Icons (system logos)", "license": "CC0-1.0", "repository": "https://github.com/simple-icons/simple-icons", "note": "assets/logos/LICENSE-SIMPLE-ICONS.md"},
    {"name": "MesloLGS NF font", "license": "Apache-2.0", "repository": "https://github.com/romkatv/powerlevel10k-media", "note": "assets/fonts/NOTICE.txt"},
    {"name": "Nerd Fonts glyphs (in MesloLGS NF)", "license": "MIT AND OFL-1.1", "repository": "https://github.com/ryanoasis/nerd-fonts", "note": "assets/fonts/LICENSE-NERD-FONTS.txt"},
]


def lock_fingerprint(text):
    """The registry and git packages of Cargo.lock, as "name version" lines
    (path packages, the app's own crates among them, left out so a version
    bump of the app does not count). Same rule as app/src/third_party.rs."""
    out = []
    for block in text.replace("\r", "").split("[[package]]")[1:]:
        fields = {}
        for line in block.split("\n"):
            if " = " in line and not line.startswith(" "):
                k, v = line.split(" = ", 1)
                fields[k.strip()] = v.strip().strip('"')
        if "source" in fields:
            out.append(f"{fields['name']} {fields['version']}")
    return sorted(out)


def fnv1a(lines):
    h = 0xCBF29CE484222325
    for b in "\n".join(lines).encode():
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


# The platforms the Platform Manager is released for.
TARGETS = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]


def build():
    seen, pkgs = set(), {}
    for t in TARGETS:
        meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", t], cwd=APP))
        pkgs.update({p["id"]: p for p in meta["packages"]})
        seen |= reachable(meta)
    return finish(seen, pkgs)


def reachable(meta):
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen, todo = set(), [root]
    while todo:
        i = todo.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            # Normal dependencies only: build scripts' and tests' crates are
            # not in the program.
            if any(k["kind"] is None for k in d["dep_kinds"]):
                todo.append(d["pkg"])
    return seen


def finish(seen, pkgs):
    crates = []
    for i in seen:
        p = pkgs[i]
        own = p["source"] is None and not p["manifest_path"].startswith(VENDOR)
        if own:
            continue
        lic = p.get("license") or ("See its licence file" if p.get("license_file") else "Not stated")
        lic = " ".join(lic.replace("/", " OR ").split())
        crates.append({"name": p["name"], "version": p["version"], "license": lic, "repository": p.get("repository") or ""})
    crates.sort(key=lambda c: (c["name"].lower(), c["version"]))
    with open(os.path.join(APP, "Cargo.lock"), encoding="utf-8") as f:
        fp = fnv1a(lock_fingerprint(f.read()))
    return {"cargo_lock": fp, "crates": crates, "assets": ASSETS}


def main():
    data = json.dumps(build(), indent=1, ensure_ascii=False) + "\n"
    if "--check" in sys.argv:
        with open(OUT, encoding="utf-8") as f:
            if f.read() != data:
                print(f"{OUT} is out of date: run scripts/third-party-licences.py", file=sys.stderr)
                sys.exit(1)
        return
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(data)
    print(f"{OUT}: {len(json.loads(data)['crates'])} crates")


if __name__ == "__main__":
    main()
