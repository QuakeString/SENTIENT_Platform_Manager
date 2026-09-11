#!/usr/bin/env bash
# Builds the .deb and the AppImage inside ubuntu:22.04 so the result runs on
# every Ubuntu from 22.04 up (and Debian 12) rather than only on this host,
# whose glibc is far newer than anything a customer will have.
set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y --no-install-recommends \
  build-essential curl ca-certificates file pkg-config \
  libssl-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev patchelf \
  desktop-file-utils xz-utils squashfs-tools zsync

export CARGO_HOME=/work/cargo RUSTUP_HOME=/work/rustup
if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path
fi
export PATH="$CARGO_HOME/bin:$PATH"
command -v cargo-tauri >/dev/null || cargo install tauri-cli --version "^2" --locked

export CARGO_TARGET_DIR=/work/target-ubuntu
# linuxdeploy needs FUSE, which a container does not have; this makes it
# unpack itself instead.
export APPIMAGE_EXTRACT_AND_RUN=1
export NO_STRIP=1

cd /work/src/src-tauri
# `active` is false in the committed config because the Windows build drives
# bundling itself; `resources` points at a pgtools directory that only exists
# on Windows. Override both for Linux rather than forking the config.
cargo tauri build --bundles deb,appimage \
  --config '{"bundle":{"active":true,"resources":[],"longDescription":"The SENTIENT Platform Manager installs the SENTIENT IIoT platform and its database, keeps them running, applies updates, and takes and restores backups. It talks to Docker on Linux and drives the compose stack the platform ships with."}}'

mkdir -p /work/out
cp -v "$CARGO_TARGET_DIR"/release/bundle/deb/*.deb /work/out/
cp -v "$CARGO_TARGET_DIR"/release/bundle/appimage/*.AppImage /work/out/
chown -R "$HOST_UID:$HOST_GID" /work/out /work/target-ubuntu /work/cargo /work/rustup
