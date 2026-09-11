#!/usr/bin/env bash
# Builds a real pacman package with makepkg in an Arch container, so the
# dependency names and the .pkg.tar.zst format come from Arch itself rather
# than from a hand-forged tarball.
set -euxo pipefail
pacman -Syu --noconfirm --needed \
  base-devel git rust pkgconf \
  webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg
id builder >/dev/null 2>&1 || useradd -m builder
# Otherwise the package records "Unknown Packager".
echo 'PACKAGER="INVENIA SYSTEMS <support@invenia.in>"' >> /etc/makepkg.conf
chown -R builder /work/arch
su builder -c 'cd /work/arch && makepkg -f --noconfirm --nodeps'
mkdir -p /work/out
cp -v /work/arch/*.pkg.tar.zst /work/out/
chown -R "$HOST_UID:$HOST_GID" /work/out /work/arch
