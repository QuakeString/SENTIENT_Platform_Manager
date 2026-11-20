#!/usr/bin/env bash
# Build `sentient-payload-linux-<arch>.tar` — the SENTIENT program files the
# Linux native engine unpacks into /opt/sentient — from a SENTIENT checkout.
#
#   packaging/linux/build-payload.sh <sentient-repo> <out-dir> [--debug] [--no-build]
#
# Layout (mirrors the container image and the Windows payload):
#   bin/sentient-server     the server (cargo example `sentient_server`)
#   bin/sentient-install    schema/system-data installer (dao bin)
#   bin/sentient-admin      local administration command (dao bin)
#   ui/                     ui-ngx/target/generated-resources/public
#   data/                   system resources, SQL, widget bundles
#   fonts/                  report fonts (installed under /usr/share/fonts)
#
# --debug uses the dev profile binaries (a test payload; very large).
# --no-build packages whatever is already built without compiling.
#
# The binaries are compiled by the SENTIENT checkout's
# scripts/build-native-linux.sh in an EL9 container (glibc 2.34), never on
# this host: a host build needs the host's glibc (4.4.0.3, built on Ubuntu
# 26.04, needed GLIBC_2.43 and failed on Ubuntu 24.04). The same binaries
# go into the Docker image (scripts/build-image.sh), so both hashes match.
# The x86_64 payload is built on an x86_64 host; arm64 needs its own run.
set -euo pipefail
repo=${1:?sentient repo path}
out=${2:?output directory}
profile=release; build=1
for a in "${@:3}"; do
  case "$a" in
    --debug) profile=debug ;;
    --no-build) build=0 ;;
    *) echo "unknown option $a" >&2; exit 2 ;;
  esac
done
arch=$(uname -m)
case "$arch" in x86_64) arch=x86_64 ;; aarch64|arm64) arch=aarch64 ;; esac

cd "$repo"
if [ "$build" = 1 ]; then
  [ -x scripts/build-native-linux.sh ] || { echo "$repo has no scripts/build-native-linux.sh (too old a checkout)" >&2; exit 1; }
  flags=(); [ "$profile" = debug ] && flags+=(--debug)
  # The licence public key is compiled in; a test payload can be pointed at a
  # local licence server with SENTIENT_TEST_LICENSE_PUBLIC_KEY (the script
  # then turns on the local-license-testing feature).
  scripts/build-native-linux.sh "${flags[@]}"
fi
bins=target/native-linux/$profile
server=$bins/examples/sentient_server
installer=$bins/sentient-install
admin=$bins/sentient-admin
ui=ui-ngx/target/generated-resources/public
for f in "$server" "$installer" "$admin" "$ui/index.html" data/sql/schema-entities.sql; do
  [ -e "$f" ] || { echo "missing $f" >&2; exit 1; }
done

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/fonts"
cp "$server" "$stage/bin/sentient-server"
cp "$installer" "$stage/bin/sentient-install"
cp "$admin" "$stage/bin/sentient-admin"
chmod 755 "$stage"/bin/*
cp -r "$ui" "$stage/ui"
cp -r data "$stage/data"
# Fonts live in per-family subdirectories; flatten them for fontconfig.
if [ -d api/assets/fonts ]; then
  find api/assets/fonts -type f \( -iname '*.ttf' -o -iname '*.otf' \) -exec cp {} "$stage/fonts/" \;
fi
mkdir -p "$out"
tar -C "$stage" -cf "$out/sentient-payload-linux-$arch.tar" bin ui data fonts
ls -la "$out/sentient-payload-linux-$arch.tar"
