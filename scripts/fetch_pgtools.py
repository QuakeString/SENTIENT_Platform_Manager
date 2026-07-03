#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
#
# Licensed under the Apache License, Version 2.0.
"""Stage portable PostgreSQL client tools for the CURRENT platform so Tauri can
bundle them as app resources (making the installer self-contained — no separate
PostgreSQL install needed on the user's machine).

Source: EnterpriseDB's "binaries only" zips, which ship pg_dump / pg_restore
together with libpq and its dependencies. We extract ONLY the two tools plus the
minimal DLL closure they need (derived via objdump import analysis — ~10 MB, not
the 60 MB of unrelated pgAdmin/ICU DLLs).

Produces:
    src-tauri/pgtools/bin/pg_dump[.exe]
    src-tauri/pgtools/bin/pg_restore[.exe]
    src-tauri/pgtools/bin/*.dll        (Windows — loaded from next to the exe)

Windows is implemented and shipped. macOS/Linux bundling is a follow-up (EDB has
no Linux zip; macOS needs dylib install-name handling); on those platforms the
app falls back to pg_dump on PATH / the SBR_PG_DUMP env var.

Usage:  python scripts/fetch_pgtools.py            # PG 18 (default)
        SBR_PG_MAJOR=18 SBR_PG_BUILD=4-1 python scripts/fetch_pgtools.py
"""
import os
import platform
import shutil
import sys
import tempfile
import urllib.request
import zipfile

PG_MAJOR = os.environ.get("SBR_PG_MAJOR", "18")
# EDB packages are versioned <major>.<minor>-<build>; pin minor.build here.
PG_BUILD = os.environ.get("SBR_PG_BUILD", "4-1")  # -> 18.4-1
EDB = "https://get.enterprisedb.com/postgresql"
HERE = os.path.dirname(os.path.abspath(__file__))
STAGE = os.path.join(HERE, "..", "src-tauri", "pgtools")

# Minimal DLL closure for pg_dump/pg_restore (objdump -p import walk over the EDB
# PG18 bin/), plus zlib1 for -Fc custom-format (gzip) compression. Names are
# stable across EDB PG18 point releases. Missing entries are skipped defensively.
WIN_DLLS = [
    "libpq.dll",
    "libssl-3-x64.dll",
    "libcrypto-3-x64.dll",
    "libintl-9.dll",
    "libiconv-2.dll",
    "libwinpthread-1.dll",
    "liblz4.dll",
    "libzstd.dll",
    "zlib1.dll",
]


def edb_zip_url(os_slug: str) -> str:
    return f"{EDB}/postgresql-{PG_MAJOR}.{PG_BUILD}-{os_slug}-binaries.zip"


def download(url: str, dest: str):
    print(f"  downloading {url}")
    with urllib.request.urlopen(url, timeout=600) as r, open(dest, "wb") as f:
        shutil.copyfileobj(r, f)
    print(f"  {os.path.getsize(dest) / 1048576:.0f} MB")


def stage_windows(tmp: str):
    zip_path = os.path.join(tmp, "pg.zip")
    download(edb_zip_url("windows-x64"), zip_path)
    dst_bin = os.path.join(STAGE, "bin")
    os.makedirs(dst_bin, exist_ok=True)  # keeps the committed .gitkeep
    wanted = {"pg_dump.exe", "pg_restore.exe", *(d.lower() for d in WIN_DLLS)}
    got = set()
    with zipfile.ZipFile(zip_path) as z:
        for n in z.namelist():
            base = n.split("/")[-1]
            if n.startswith("pgsql/bin/") and base.lower() in wanted:
                with z.open(n) as src, open(os.path.join(dst_bin, base), "wb") as out:
                    shutil.copyfileobj(src, out)
                got.add(base.lower())
    for tool in ("pg_dump.exe", "pg_restore.exe"):
        if tool not in got:
            sys.exit(f"FATAL: {tool} not found in EDB zip")
    missing = wanted - got
    if missing:
        print(f"  note: DLLs not present (may be fine): {sorted(missing)}")
    total = sum(
        os.path.getsize(os.path.join(dst_bin, f)) for f in os.listdir(dst_bin)
    )
    print(f"  staged {len(got)} files, {total / 1048576:.1f} MB -> {os.path.normpath(dst_bin)}")


def main():
    sysname = platform.system()
    print(f"pg tools: PG {PG_MAJOR}.{PG_BUILD} for {sysname}")
    if sysname != "Windows":
        # Not yet bundled here — leave the committed .gitkeep so the Tauri
        # resource glob still matches; the app falls back to pg_dump on PATH.
        print(f"  {sysname}: bundling not implemented yet; skipping (PATH fallback).")
        return
    with tempfile.TemporaryDirectory() as tmp:
        stage_windows(tmp)


if __name__ == "__main__":
    main()
