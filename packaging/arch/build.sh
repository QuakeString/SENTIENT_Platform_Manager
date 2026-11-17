#!/usr/bin/env bash
# Build sentient-platform-manager-<version>-1-x86_64.pkg.tar.zst on Arch Linux
# (or in an archlinux container), as a non-root user with base-devel, rust,
# clang, cmake and the libraries GPUI compiles against (fontconfig, freetype2,
# libxcb, libxkbcommon(-x11), wayland, vulkan-icd-loader, alsa-lib) installed.
# The version comes from SPM_VERSION (e.g. 0.8.0-rc12; dashes dropped for
# pkgver, as build-cached.sh does), else app/Cargo.toml; the release number
# from SPM_PKGREL (default 1).
#   packaging/arch/build.sh [<out-dir>]
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/out}
v=${SPM_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../app/Cargo.toml" | head -1)}
v=${v//-/}
rel=${SPM_PKGREL:-1}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sed "s/^pkgver=.*/pkgver=$v/; s/^pkgrel=.*/pkgrel=$rel/" "$here/PKGBUILD" > "$work/PKGBUILD.in"
# makepkg runs in the directory of the PKGBUILD; keep it here so $startdir
# points into the checkout.
cp "$work/PKGBUILD.in" "$here/.PKGBUILD.$v"
cd "$here"
BUILDDIR="$work/build" PKGDEST="$out" SRCDEST="$work/src" makepkg -f --noconfirm --nodeps -p ".PKGBUILD.$v"
rm -f "$here/.PKGBUILD.$v"
ls -la "$out"
