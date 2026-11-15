#!/usr/bin/env bash
# A quick Arch package between releases: the same PKGBUILD, in a builder
# image made once, with the cargo registry and the target folder kept
# between builds (a small change rebuilds in minutes). Tagged releases keep
# the clean build (packaging/arch/build.sh in a fresh archlinux container).
#
#   packaging/arch/build-cached.sh <commit> <pkgrel> [<out-dir>]
#
# SPM_VERSION (default from app/Cargo.toml) and SPM_CACHE (default
# ~/.cache/spm-archbuild) can be set. The package is built from
# `git archive <commit>`, so uncommitted changes never reach it.
set -euo pipefail
commit=${1:?commit}
pkgrel=${2:?pkgrel}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${3:-$here/out}
cache=${SPM_CACHE:-$HOME/.cache/spm-archbuild}
version=${SPM_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/app/Cargo.toml" | head -1)}
pkgver=${version//-/}
short=$(git -C "$repo" rev-parse --short=7 "$commit")
mkdir -p "$cache/cargo" "$cache/target" "$out"
docker build -q -t spm-archbuild:latest -f "$here/Dockerfile.build" "$here" >/dev/null
work=$(mktemp -d "$cache/work.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/src"
git -C "$repo" archive "$commit" | tar -x -C "$work/src"
sed -i "s/^pkgver=.*/pkgver=$pkgver/; s/^pkgrel=.*/pkgrel=$pkgrel/" "$work/src/packaging/arch/PKGBUILD"
docker run --rm \
  -v "$work:/w" -v "$cache/cargo:/cache/cargo" -v "$cache/target:/cache/target" -v "$out:/out" \
  -u builder -e HOME=/home/builder \
  -e CARGO_HOME=/cache/cargo -e SPM_CARGO_TARGET_DIR=/cache/target \
  -e SPM_VERSION="$version" -e SPM_COMMIT="$short" \
  spm-archbuild:latest bash -c 'cd /w/src/packaging/arch && BUILDDIR=/tmp/build PKGDEST=/out SRCDEST=/tmp/src makepkg -f --noconfirm --nodeps'
ls -la "$out"/sentient-platform-manager-"$pkgver"-"$pkgrel"-*.pkg.tar.zst
