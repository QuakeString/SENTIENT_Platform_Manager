#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Sign release files with the SENTIENT release key (minisign format, ed25519):
# writes <file>.minisig beside each file, then checks it against the public
# key in this repository (packaging/release-signing.pub, the key the Platform
# Manager pins in crates/sentient-installer-core/src/signing.rs).
#
#   scripts/sign-release.sh <file>...
#
# What gets signed (the publish scripts call this):
#   - a SENTIENT release's manifest.json (it lists every file's SHA-256);
#   - each Platform Manager installer (SENTIENT-Setup-manager-*).
#
# The secret key never enters a repository. It is read from
# SENTIENT_RELEASE_KEY, default ~/.sentient-keys/release-2026-11/release.key
# (folder 0700, file 0600, on the release machine only). Signing needs
# rsign (cargo install rsign2) or minisign on PATH.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
key=${SENTIENT_RELEASE_KEY:-$HOME/.sentient-keys/release-2026-11/release.key}
pub="$root/packaging/release-signing.pub"
[ $# -gt 0 ] || { echo "usage: $0 <file>..." >&2; exit 2; }
[ -r "$key" ] || { echo "no release key at $key (set SENTIENT_RELEASE_KEY)" >&2; exit 2; }
[ -r "$pub" ] || { echo "no public key at $pub" >&2; exit 2; }
# A key anyone else on this machine can read is not a secret any more.
mode=$(stat -c '%a' "$key" 2>/dev/null || stat -f '%Lp' "$key")
case "$mode" in 600|400) ;; *) echo "$key is mode $mode: make it 0600 first" >&2; exit 2 ;; esac

if command -v rsign >/dev/null 2>&1; then
  sign() { rsign sign -W -s "$key" -t "file:$(basename "$1")" -x "$1.minisig" "$1" >/dev/null; }
  check() { rsign verify -q -p "$pub" -x "$1.minisig" "$1" >/dev/null; }
elif command -v minisign >/dev/null 2>&1; then
  sign() { minisign -S -s "$key" -t "file:$(basename "$1")" -x "$1.minisig" -m "$1" </dev/null >/dev/null; }
  check() { minisign -V -q -p "$pub" -x "$1.minisig" -m "$1" >/dev/null; }
else
  echo "neither rsign (cargo install rsign2) nor minisign is on PATH" >&2; exit 2
fi

for f in "$@"; do
  [ -f "$f" ] || { echo "$f: no such file" >&2; exit 1; }
  sign "$f"
  check "$f" || { echo "$f.minisig does not verify with $pub" >&2; exit 1; }
  echo "signed $(basename "$f") -> $(basename "$f").minisig"
done
