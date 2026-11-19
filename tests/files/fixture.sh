#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# The click test's folders (tests/files/clicks.py), here or on a server:
#
#   tests/files/fixture.sh make|remove <base folder> [ssh arguments…]
#
# <base>/big: dir-01 … dir-40 (two files each; dir-01/deeper) and 60 files
# with a link to dir-01; <base>/fresh: three files; <base>/locked: mode 000.
# With ssh arguments (e.g. -p 2242 -i key ops@host) on that server.
set -eu
what=${1:?make|remove}; base=${2:?base folder}; shift 2
script=$(cat <<'EOF'
set -eu
b=$1
if [ -d "$b/locked" ]; then chmod 700 "$b/locked"; fi
rm -rf "$b"
[ "$2" = remove ] && exit 0
mkdir -p "$b/big" "$b/fresh" "$b/locked"
cd "$b/big"
for i in $(seq -w 1 40); do mkdir "dir-$i"; : > "dir-$i/inner-a"; : > "dir-$i/inner-b"; done
mkdir dir-01/deeper
for i in $(seq -w 1 60); do head -c $((i * 997)) /dev/zero > "file-$i.bin"; done
ln -s dir-01 link-to-dir-01
for i in 1 2 3; do : > "$b/fresh/note-$i.txt"; done
chmod 000 "$b/locked"
EOF
)
if [ $# -gt 0 ]; then
  ssh -o BatchMode=yes -o LogLevel=ERROR "$@" "bash -s -- '$base' '$what'" <<<"$script"
else
  bash -c "$script" fixture "$base" "$what"
fi
