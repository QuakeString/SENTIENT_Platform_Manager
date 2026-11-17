#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Build the Platform Manager Flatpak bundle from an already built GUI binary:
#
#   packaging/flatpak/build.sh <binary> <version> <out-dir>
#     -> <out-dir>/sentient-platform-manager-<version>-x86_64.flatpak
#
# Needs flatpak-builder with org.freedesktop.Sdk//24.08 and
# org.freedesktop.Platform//24.08 installed (CI: the flathub-infra
# flatpak-github-actions:freedesktop-24.08 container). The bundle names
# Flathub as the place its runtime comes from, so `flatpak install
# <bundle>` fetches it there when the computer lacks it.
set -euo pipefail
bin=${1:?usage: $0 <binary> <version> <out-dir>}
version=${2:?version}
out=${3:?out-dir}
here=$(cd "$(dirname "$0")" && pwd)
id=in.invenia.SentientPlatformManager
[ -s "$bin" ] || { echo "error: missing $bin" >&2; exit 1; }
for f in "$here/$id.yml" "$here/$id.desktop" "$here/$id.metainfo.xml.in"; do
  [ -s "$f" ] || { echo "error: missing $f" >&2; exit 1; }
done
mkdir -p "$out"
out=$(cd "$out" && pwd)

rm -rf "$here/input"
mkdir -p "$here/input"
install -m755 "$bin" "$here/input/sentient-platform-manager"
sed "s/@VERSION@/$version/; s/@DATE@/$(date -u +%Y-%m-%d)/" "$here/$id.metainfo.xml.in" > "$here/input/$id.metainfo.xml"

work=${FLATPAK_WORK:-$(mktemp -d)}
cd "$here"
flatpak-builder --disable-rofiles-fuse --force-clean --repo="$work/repo" --state-dir="$work/state" "$work/build" "$id.yml"
bundle="$out/sentient-platform-manager-$version-x86_64.flatpak"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo "$work/repo" "$bundle" "$id"
[ -s "$bundle" ] || { echo "error: no bundle at $bundle" >&2; exit 1; }
rm -rf "$here/input"
ls -la "$bundle"
