#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Package an already built Platform Manager (the GUI binary from the
# gpui-linux CI job, with its server agents inside) for Linux distributions
# other than Arch (packaging/arch has its own PKGBUILD):
#
#   packaging/linux/build-packages.sh <binary> <version> <out-dir> <kind>...
#
#   kind  deb       sentient-platform-manager_<debver>_amd64.deb       (nfpm)
#         rpm       sentient-platform-manager-<rpmver>-1.x86_64.rpm   (nfpm)
#         appimage  sentient-platform-manager-<version>-x86_64.AppImage (appimagetool)
#
# <version> is the program's own (0.9.1, 0.9.1-rc4, 0.9.1-nightly.20261117).
# The packages sort a pre-release before its final version the way each
# package manager does it: 0.9.1-rc4 becomes 0.9.1~rc4 for dpkg and rpm.
#
# Every package installs the same files as the Arch one, where the program
# installs itself (crate::self_install): /opt/sentient-platform-manager/
# sentient-platform-manager, the command /usr/bin/sentient-platform-manager,
# the desktop entry (packaging/arch/sentient-platform-manager.desktop), the
# icons in every size and the licence. The server agents are inside the
# binary (build.rs); `--job agents` lists them.
#
# The library dependencies come from the binary itself: dpkg-shlibdeps for
# the .deb (run this on the oldest Ubuntu supported, 22.04, like the build),
# the ELF NEEDED entries as sonames for the .rpm (Fedora, RHEL and openSUSE
# all provide them), plus the libraries GPUI opens at run time (Vulkan,
# Wayland), which no tool can see.
#
# Needs nfpm (deb, rpm), dpkg-dev (deb), binutils, and appimagetool
# (appimage; NFPM / APPIMAGETOOL name them when they are not on PATH).
set -euo pipefail
bin=${1:?usage: $0 <binary> <version> <out-dir> <kind>...}
version=${2:?version}
out=${3:?out-dir}
shift 3
[ $# -gt 0 ] || { echo "name at least one kind: deb rpm appimage" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
name=sentient-platform-manager
desktop="$repo/packaging/arch/$name.desktop"
icons="$repo/app/assets/app-icon"
licence="$repo/app/assets/licence.txt"
sizes="16 22 24 32 48 64 128 256 512"

die() { echo "error: $*" >&2; exit 1; }
need_file() { [ -s "$1" ] || die "missing $1"; }
need_file "$bin"
need_file "$desktop"
need_file "$licence"
need_file "$icons/spm-app-icon.svg"
for n in $sizes; do need_file "$icons/$n.png"; done
file -L "$bin" | grep -q 'ELF 64-bit.*x86-64' || die "$bin is not an x86_64 ELF program"

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# 0.9.1-rc4 -> 0.9.1~rc4 (the first dash only: the version has no Debian
# revision of its own).
pkgver=${version/-/\~}

# The installed tree, shared by the .deb and the .rpm.
stage() {
  local root=$1
  install -Dm755 "$bin" "$root/opt/$name/$name"
  install -dm755 "$root/usr/bin"
  ln -s "/opt/$name/$name" "$root/usr/bin/$name"
  install -Dm644 "$desktop" "$root/usr/share/applications/$name.desktop"
  local n
  for n in $sizes; do
    install -Dm644 "$icons/$n.png" "$root/usr/share/icons/hicolor/${n}x${n}/apps/$name.png"
  done
  install -Dm644 "$icons/spm-app-icon.svg" "$root/usr/share/icons/hicolor/scalable/apps/$name.svg"
  install -Dm644 "$licence" "$root/usr/share/licenses/$name/LICENSE"
}

# Debian package names for the libraries the binary links against.
deb_depends() {
  local d="$work/shlibdeps"
  mkdir -p "$d/debian"
  printf 'Source: %s\n\nPackage: %s\nArchitecture: amd64\n' "$name" "$name" > "$d/debian/control"
  cp "$bin" "$d/$name"
  local line
  line=$(cd "$d" && dpkg-shlibdeps -O -e"$name" 2>"$d/warnings") || { cat "$d/warnings" >&2; die "dpkg-shlibdeps failed"; }
  line=${line#shlibs:Depends=}
  [ -n "$line" ] || die "dpkg-shlibdeps found no libraries"
  # The libraries GPUI loads itself (dlopen): the Vulkan loader and the
  # Wayland client library.
  printf '%s\n' "$line" | tr ',' '\n' | sed 's/^ *//; s/ *$//' | grep -v '^$'
  printf '%s\n' libvulkan1 libwayland-client0
}

# rpm capabilities: one soname()(64bit) per NEEDED library, and the newest
# glibc symbol version the binary uses.
rpm_requires() {
  local libs glibc
  libs=$(objdump -p "$bin" | awk '/^ *NEEDED/{print $2}')
  [ -n "$libs" ] || die "no NEEDED libraries in $bin"
  for l in $libs; do printf '%s()(64bit)\n' "$l"; done
  glibc=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sort -uV | tail -1)
  [ -n "$glibc" ] && printf 'libc.so.6(%s)(64bit)\n' "$glibc"
  printf '%s\n' 'libvulkan.so.1()(64bit)' 'libwayland-client.so.0()(64bit)'
}

yaml_list() { sed 's/.*/  - "&"/'; }

nfpm_package() {
  local kind=$1 target=$2 deps=$3
  local root="$work/root"
  rm -rf "$root"
  stage "$root"
  local nfpm=${NFPM:-nfpm}
  command -v "$nfpm" >/dev/null || die "nfpm is not installed"
  local recommends suggests
  if [ "$kind" = deb ]; then
    recommends=$'pkexec | policykit-1\nmesa-vulkan-drivers | vulkan-icd'
    suggests=$'docker.io | docker-ce\npostgresql\nzenity | kdialog'
  else
    recommends=$'polkit\nmesa-vulkan-drivers'
    suggests=$'docker\npostgresql-server\nzenity'
  fi
  cat > "$work/nfpm.yaml" <<EOF
name: $name
arch: amd64
platform: linux
version: "$version"
version_schema: semver
release: 1
section: admin
priority: optional
maintainer: "INVENIA SYSTEMS <support@invenia.in>"
vendor: INVENIA SYSTEMS
homepage: https://inveniasystems.com
license: LicenseRef-SENTIENT-EULA
description: |-
  SENTIENT Platform Manager
  Installs, runs, updates and backs up the SENTIENT platform, on this
  computer and on the servers it manages.
depends:
$(printf '%s\n' "$deps" | yaml_list)
recommends:
$(printf '%s\n' "$recommends" | yaml_list)
suggests:
$(printf '%s\n' "$suggests" | yaml_list)
contents:
  - src: $root/opt/$name/$name
    dst: /opt/$name/$name
    file_info: { mode: 0755 }
  - src: /opt/$name/$name
    dst: /usr/bin/$name
    type: symlink
$(cd "$root" && find usr/share -type f | sort | sed "s|.*|  - src: $root/&\n    dst: /&\n    file_info: { mode: 0644 }|")
rpm:
  group: Applications/System
  summary: Install, run, update and back up the SENTIENT platform
EOF
  echo "--- $kind dependencies"; printf '%s\n' "$deps"
  "$nfpm" package --config "$work/nfpm.yaml" --packager "$kind" --target "$target"
  need_file "$target"
}

appimage() {
  local target=$1 dir="$work/AppDir"
  rm -rf "$dir"
  install -Dm755 "$bin" "$dir/usr/bin/$name"
  local n
  for n in $sizes; do
    install -Dm644 "$icons/$n.png" "$dir/usr/share/icons/hicolor/${n}x${n}/apps/$name.png"
  done
  install -Dm644 "$icons/spm-app-icon.svg" "$dir/usr/share/icons/hicolor/scalable/apps/$name.svg"
  install -Dm644 "$licence" "$dir/usr/share/licenses/$name/LICENSE"
  cp "$icons/256.png" "$dir/$name.png"
  ln -s "$name.png" "$dir/.DirIcon"
  # The same entry, run from inside the AppImage (no /opt path).
  sed "s|^Exec=/opt/$name/$name|Exec=$name|" "$desktop" > "$dir/$name.desktop"
  install -Dm644 "$dir/$name.desktop" "$dir/usr/share/applications/$name.desktop"
  grep -q "^Exec=$name --manager" "$dir/$name.desktop" || die "the AppImage desktop entry was not rewritten"
  # Libraries are not bundled: the AppImage needs what the plain Linux
  # binary needs (fontconfig, xkbcommon, Wayland/X11, Vulkan), which every
  # desktop has, and the program copied out of it by "install the Platform
  # Manager" runs the same way.
  cat > "$dir/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/sentient-platform-manager" "$@"
EOF
  chmod 755 "$dir/AppRun"
  local tool=${APPIMAGETOOL:-appimagetool}
  command -v "$tool" >/dev/null || [ -x "$tool" ] || die "appimagetool is not installed"
  ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 VERSION="$version" "$tool" --no-appstream "$dir" "$target"
  need_file "$target"
  chmod 755 "$target"
}

for kind in "$@"; do
  case "$kind" in
    # Each list in its own assignment, so a failure stops the build.
    deb) deps=$(deb_depends); nfpm_package deb "$out/${name}_${pkgver}_amd64.deb" "$deps" ;;
    rpm) deps=$(rpm_requires); nfpm_package rpm "$out/${name}-${pkgver}-1.x86_64.rpm" "$deps" ;;
    appimage) appimage "$out/${name}-${version}-x86_64.AppImage" ;;
    *) die "unknown kind $kind (deb, rpm, appimage)" ;;
  esac
done
ls -la "$out"
