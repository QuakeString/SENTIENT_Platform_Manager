#!/usr/bin/env bash
# Build `sentient-payload-linux-<arch>.tar` — the SENTIENT program files the
# Linux native engine unpacks into /opt/sentient — from a SENTIENT checkout.
#
#   packaging/linux/build-payload.sh <sentient-repo> <out-dir> [--debug] [--no-build]
#
# Layout (mirrors the container image and the Windows payload):
#   bin/sentient-server     the server (cargo example `sentient_server`)
#   bin/sentient-install    schema/system-data installer (dao bin)
#   ui/                     ui-ngx/target/generated-resources/public
#   data/                   system resources, SQL, widget bundles
#   fonts/                  report fonts (installed under /usr/share/fonts)
#
# --debug uses the dev profile binaries (a test payload; very large).
# --no-build packages whatever is already in target/ without compiling.
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
  flags=(); [ "$profile" = release ] && flags+=(--release)
  # The licence public key is compiled in; a test payload can be pointed at a
  # local licence server with SENTIENT_TEST_LICENSE_PUBLIC_KEY + this feature.
  [ -n "${SENTIENT_TEST_LICENSE_PUBLIC_KEY:-}" ] && flags+=(--features local-license-testing)
  cargo build "${flags[@]}" -p sentient-api --example sentient_server
  cargo build "${flags[@]}" -p sentient-dao --bin sentient-install
fi
server=target/$profile/examples/sentient_server
installer=target/$profile/sentient-install
ui=ui-ngx/target/generated-resources/public
for f in "$server" "$installer" "$ui/index.html" data/sql/schema-entities.sql; do
  [ -e "$f" ] || { echo "missing $f" >&2; exit 1; }
done

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/fonts"
cp "$server" "$stage/bin/sentient-server"
cp "$installer" "$stage/bin/sentient-install"
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
