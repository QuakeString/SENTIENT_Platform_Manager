#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Sign the SENTIENT programs inside a Windows payload, in place:
#
#   scripts/sign-sentient-payload.sh <folder>/sentient-payload.tar
#   scripts/sign-sentient-payload.sh --check <folder>/sentient-payload.tar
#
# bin/sentient-server.exe, bin/sentient-install.exe and bin/sentient-admin.exe
# are signed with scripts/sign-windows.sh (its certificate settings); every
# other entry is copied as it is (services/SentientServer.exe is the
# third-party service wrapper, signed or not by its own publisher). The tar
# keeps its order, names and modes. Run it before publish-sentient-release.sh,
# which refuses an unsigned Windows server while a certificate is configured.
set -euo pipefail
check=false
[ "${1:-}" = "--check" ] && { check=true; shift; }
tar=${1:?usage: $0 [--check] <sentient-payload.tar>}
[ -f "$tar" ] || { echo "no $tar" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
progs=(bin/sentient-server.exe bin/sentient-install.exe bin/sentient-admin.exe)
work=$(mktemp -d -p "$(dirname "$tar")")
trap 'rm -rf "$work"' EXIT
tar -C "$work" -xf "$tar" "${progs[@]}"
if $check; then
  "$here/sign-windows.sh" --check "${progs[@]/#/$work/}"; exit $?
fi
"$here/sign-windows.sh" --desc "SENTIENT Server" "$work/bin/sentient-server.exe"
"$here/sign-windows.sh" --desc "SENTIENT install" "$work/bin/sentient-install.exe"
"$here/sign-windows.sh" --desc "SENTIENT admin" "$work/bin/sentient-admin.exe"
# Replace the three entries where they are: --update would append copies.
python3 -I - "$tar" "$work" "${progs[@]}" <<'PY'
import os, sys, tarfile
src, work, progs = sys.argv[1], sys.argv[2], set(sys.argv[3:])
out = src + ".signing"
with tarfile.open(src) as t, tarfile.open(out, "w", format=t.format) as o:
    for m in t:
        if m.name in progs:
            p = os.path.join(work, m.name)
            m.size = os.path.getsize(p)
            with open(p, "rb") as f:
                o.addfile(m, f)
        elif m.isfile():
            o.addfile(m, t.extractfile(m))
        else:
            o.addfile(m)
os.chmod(out, os.stat(src).st_mode & 0o7777)
os.replace(out, src)
PY
"$0" --check "$tar"
