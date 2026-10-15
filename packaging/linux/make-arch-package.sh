#!/usr/bin/env bash
# Build the Arch package from the current working tree, in a throwaway Arch
# container (see build-arch.sh for why). Output lands in <work>/out.
#
#   packaging/linux/make-arch-package.sh [work-dir]     (default ~/INVENIA/spm-linux)
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
W=${1:-$HOME/INVENIA/spm-linux}
pkgver=$(sed -n 's/^pkgver=//p' "$repo/packaging/linux/PKGBUILD")
mkdir -p "$W/arch" "$W/out"
# The working tree as it is now (not HEAD), minus build output. A previous
# in-container build can leave root-owned files in src/, so remove it as root.
[ -d "$W/src" ] && docker run --rm -v "$W:/work" archlinux:latest rm -rf /work/src
rsync -a --delete \
  --exclude target --exclude 'src-tauri/target' --exclude node_modules --exclude .git \
  "$repo/" "$W/src/"
tar -czf "$W/arch/spm-$pkgver.tar.gz" -C "$W" --transform "s,^src,spm-$pkgver," src
cp "$repo/packaging/linux/PKGBUILD" "$repo/packaging/linux/sentient-platform-manager.desktop" "$W/arch/"
cp "$repo/packaging/linux/build-arch.sh" "$W/"
rm -f "$W"/out/*.pkg.tar.zst
docker run --rm -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" -v "$W:/work" archlinux:latest bash /work/build-arch.sh
ls -la "$W"/out/*.pkg.tar.zst
