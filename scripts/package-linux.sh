#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Build the SENTIENT Setup package for Linux:
#
#   SENTIENT-Setup-<version>-linux-<arch>/
#     SENTIENT-Setup                     the GPUI installer + Platform Manager
#     payload/
#       sentient-payload-linux-<arch>.tar  SENTIENT server, UI and data
#       VERSION                            the SENTIENT version, shown by setup
#
# and tar it. Setup finds payload/ next to itself: the native (systemd) mode
# installs from it without downloading SENTIENT; the Docker mode pulls the
# image. Build the payload with packaging/linux/build-payload.sh first.
#
# A binary linked against a newer glibc will not start on an older distro:
# build on the oldest distro you support (CI uses ubuntu-22.04).
#
# Usage: scripts/package-linux.sh <sentient-payload-linux-<arch>.tar> <sentient-version> [out-dir]
set -euo pipefail

payload_tar=${1:?payload tar}
version=${2:?SENTIENT version}
out=${3:-dist}
arch=$(uname -m)

[ -f "$payload_tar" ] || { echo "missing $payload_tar" >&2; exit 1; }
expected="sentient-payload-linux-${arch}.tar"
[ "$(basename "$payload_tar")" = "$expected" ] || { echo "the payload must be named $expected" >&2; exit 1; }

(cd app && cargo build --release)
bin=app/target/release/sentient-platform-manager

name="SENTIENT-Setup-${version}-linux-${arch}"
dir="$out/$name"
rm -rf "$dir"
mkdir -p "$dir/payload"
install -m755 "$bin" "$dir/SENTIENT-Setup"
cp "$payload_tar" "$dir/payload/"
printf '%s\n' "$version" > "$dir/payload/VERSION"

tar -C "$out" -czf "$out/$name.tar.gz" "$name"
echo "Built $out/$name.tar.gz"
