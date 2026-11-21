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
#
# The package carries every server agent (crate::remote): x86_64 Linux is
# built by the PKGBUILD; Windows (x86_64-pc-windows-gnu) and ARM64 Linux
# (aarch64-unknown-linux-musl) need cross toolchains, so they are built
# here first in the cross-rs images, with this computer's rustup toolchain
# (RUSTUP_HOME, default ~/.rustup; SPM_RUSTUP_TOOLCHAIN, default stable)
# when it has those targets. One it cannot build is left out, and the
# PKGBUILD says so.
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

# The cross-built agents (see above), by their release names.
agents="$work/agents"
mkdir -p "$agents"
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
toolchain=${SPM_RUSTUP_TOOLCHAIN:-stable}
tc_dir=$(ls -d "$rustup_home/toolchains/$toolchain"* 2>/dev/null | head -1 || true)
cross_agent() { # <target> <image> <release name> <built file> [setup shell]
  local target=$1 image=$2 name=$3 file=$4 setup=${5:-true}
  if [ -z "$tc_dir" ] || [ ! -d "$tc_dir/lib/rustlib/$target" ]; then
    echo "No rustup toolchain with $target here (rustup target add $target): $name is left out." >&2
    return 0
  fi
  mkdir -p "$cache/target-$target" "$cache/extra-$target"
  if docker run --rm -u "$(id -u):$(id -g)" \
      -v "$work/src:/src" -v "$tc_dir:/toolchain:ro" -v "$cache/cargo:/cache/cargo" \
      -v "$cache/target-$target:/t" -v "$cache/extra-$target:/extra" \
      -e HOME=/tmp ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS} -e CARGO_HOME=/cache/cargo -e PATH="/toolchain/bin:/usr/local/bin:/usr/bin:/bin" \
      -e SPM_VERSION="$version" -e SPM_COMMIT="$short" -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-6}" \
      -w /src/app "$image" sh -c "set -e; $setup; cargo build --release --locked --no-default-features --target $target --target-dir /t"; then
    cp "$cache/target-$target/$target/release/$file" "$agents/$name"
  else
    echo "The $target agent did not build: $name is left out." >&2
  fi
}
# The mingw of the cross-rs image lacks libsynchronization.a, which Rust's
# std links (WaitOnAddress and friends): an import library for it.
win_setup='if [ ! -e /usr/x86_64-w64-mingw32/lib/libsynchronization.a ]; then printf "LIBRARY api-ms-win-core-synch-l1-2-0.dll\nEXPORTS\nWaitOnAddress\nWakeByAddressSingle\nWakeByAddressAll\n" > /extra/sync.def && x86_64-w64-mingw32-dlltool -d /extra/sync.def -l /extra/libsynchronization.a; fi; export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L /extra"'
cross_agent x86_64-pc-windows-gnu ghcr.io/cross-rs/x86_64-pc-windows-gnu:0.2.5 \
  sentient-platform-manager-agent-windows-x86_64.exe sentient-platform-manager.exe "$win_setup"
cross_agent aarch64-unknown-linux-musl ghcr.io/cross-rs/aarch64-unknown-linux-musl:0.2.5 \
  sentient-platform-manager-agent-linux-aarch64 sentient-platform-manager

docker run --rm \
  -v "$work:/w" -v "$cache/cargo:/cache/cargo" -v "$cache/target:/cache/target" -v "$out:/out" \
  -u builder -e HOME=/home/builder \
  ${CARGO_BUILD_JOBS:+-e CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS} -e CARGO_HOME=/cache/cargo -e SPM_CARGO_TARGET_DIR=/cache/target \
  -e SPM_VERSION="$version" -e SPM_COMMIT="$short" -e SPM_PREBUILT_AGENTS=/w/agents \
  spm-archbuild:latest bash -c 'cd /w/src/packaging/arch && BUILDDIR=/tmp/build PKGDEST=/out SRCDEST=/tmp/src makepkg -f --noconfirm --nodeps'
ls -la "$out"/sentient-platform-manager-"$pkgver"-"$pkgrel"-*.pkg.tar.zst
