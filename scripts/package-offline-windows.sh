#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Build the Windows x64 offline pack of the Platform Manager
# (docs/analysis/OFFLINE_INSTALLER_STUDY.md), on Linux:
#
#   SPM-offline-installer-<spm>-SENTIENT-<ver>-windows-x64.zip  (+ .zip.sha256)
#
# The zip holds one folder: pack.json, pack.json.minisig and every file
# pack.json lists (crates/sentient-installer-core/src/pack.rs reads it):
#
#   SENTIENT-Setup.exe                       spm
#   EULA.txt  README.txt                     other
#   payload/sentient-payload.tar             payload
#   payload/postgresql-*-binaries.zip        pg
#   payload/timescaledb-*.zip                tsdb
#   payload/vc_redist.x64.exe                vcredist
#   payload/VERSION  payload/NOTES.md        other
#   gateway/SentientGatewaySetup-<v>.exe     gateway
#   wsl/wsl.<v>.x64.msi                      wsl_msi
#   wsl/wsl_update_x64.msi                   kernel_msi
#   browser/googlechromestandaloneenterprise64.msi    chrome_msi (the kiosk's browser)
#   wsl/ubuntu-noble-wsl-amd64-<date>.rootfs.tar.gz   rootfs
#   wsl/docker-debs/NN-<pkg>_<ver>_<arch>.deb         deb (pack.json order = install order)
#   images/<name>.tar.zst                    image (image_ref, image_digest)
#
# Usage:
#   scripts/package-offline-windows.sh --spm-exe <SENTIENT-Setup-manager-X.exe> \
#       --sentient-release <dir with manifest.json> --gateway <version|latest> \
#       --key <minisign secret key> [--pub <public key>] [options]
#
# Options:
#   --spm-exe <path>          the Platform Manager's Windows setup exe
#   --spm-version <v>         default: from the exe's name (SENTIENT-Setup-manager-<v>.exe)
#   --spm-sha256 <hex>        the exe's expected SHA-256; default: a SHA256SUMS or
#                             <exe>.sha256 beside it that lists it (else it fails)
#   --sentient-release <dir>  holds manifest.json(.minisig), sentient-payload.tar,
#                             the PostgreSQL and TimescaleDB zips, vc_redist.x64.exe
#   --gateway <v|latest>      gateway-<v> release of QuakeString/sentient-release
#   --key <path>              minisign/rsign secret key (mode 0600, no password)
#   --pub <path>              its public key (default packaging/release-signing.pub)
#   --work <dir>              default /mnt/data_1/INVENIA/spm-wip/offline-build
#   --out <dir>               where the zip goes (default <work>/out)
#   --github-curl-config <f>  a curl config (mode 0600) with the GitHub token, e.g.
#                             header = "Authorization: Bearer <token>"
#                             default ~/.config/sentient-release/github.curl when it exists;
#                             else anonymous (public downloads, rate limited)
#   --chrome-sha256 <hex>     the Chrome Enterprise MSI's SHA-256 when Google has
#                             published a newer one than CHROME_MSI_SHA256 below
#                             (check it on Windows: Get-AuthenticodeSignature says
#                             Valid, O=Google LLC), else the build stops
#   --keep-staging            keep the unzipped folder (hard links into the cache)
#
# The release pack MUST be signed with the SENTIENT release key (the one whose
# public half is packaging/release-signing.pub and RELEASE_PUBLIC_KEY in
# crates/sentient-installer-core/src/signing.rs): a pack signed with another
# key is refused by the Platform Manager. A test key works with --pub <its .pub>
# and the verify-pack test only.
#
# Idempotent: everything is downloaded or built into <work>/cache once and
# reused while its SHA-256 still matches. Every input is checked against a
# pinned or published SHA-256; any mismatch stops the build.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)

# ---------------------------------------------------------------------------
# Pinned third-party inputs. The online install takes "latest"/"current" of
# each (crates/sentient-installer-core/src/wsl.rs latest_package_url,
# distro.rs ROOTFS_URL, wsl.rs KERNEL_MSI_X64); the pack freezes them.
# ---------------------------------------------------------------------------

# Microsoft's WSL package (Kind::WslMsi): github.com/microsoft/WSL release,
# the same URL wsl.rs package_url_for_tag builds. SHA-256 = the release
# asset's digest on GitHub.
WSL_VERSION=3.0.5
WSL_MSI_URL="https://github.com/microsoft/WSL/releases/download/${WSL_VERSION}/wsl.${WSL_VERSION}.0.x64.msi"
WSL_MSI_SHA256=2313efc5873e3f8552f42e77f950fabd913c0289078fa89d2100f3421a525eaa

# The WSL 2 kernel for a Windows 10 older than .2311 (Kind::KernelMsi):
# wsl.rs KERNEL_MSI_X64. Unversioned upstream (Last-Modified 2021-10-04,
# Content-MD5 ZV6RfxGC1lrkKnDq2u2LiA==); SHA-256 of that file.
KERNEL_MSI_URL="https://wslstorestorage.blob.core.windows.net/wslblob/wsl_update_x64.msi"
KERNEL_MSI_SHA256=4d09c776c8d45f70a202281d18e19be1118f53159b0c217a5274a31ce18525fe

# Google Chrome Enterprise (Kind::ChromeMsi): the kiosk's browser on a
# computer with none (kiosk.rs CHROME_MSI, which the online install
# downloads). Unversioned upstream: Google replaces it with every Chrome
# release, so a new pack needs a new pin (or --chrome-sha256). This one:
# Last-Modified 2026-11-18 18:12:12 GMT, ETag "71d5109", 175902720 bytes.
# Setup checks Google's Authenticode signature before it installs it.
CHROME_MSI_URL="https://dl.google.com/dl/chrome/install/googlechromestandaloneenterprise64.msi"
CHROME_MSI_SHA256=3a6e02f372f35dd9f97b4109440c5622e02db4c42406a7fd99d08145a5276ba0

# The Ubuntu 24.04 WSL root file system (Kind::Rootfs). distro.rs ROOTFS_URL
# is releases/24.04/current/…; current is this dated release (same SHA-256
# in both SHA256SUMS on 2026-11-19).
UBUNTU_RELEASE=24.04
UBUNTU_CODENAME=noble
ROOTFS_DATE=20240423
ROOTFS_URL="https://cloud-images.ubuntu.com/wsl/releases/${UBUNTU_RELEASE}/${ROOTFS_DATE}/ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz"
ROOTFS_SHA256=2a790896740b14d637dbdc583cce1ba081ac53b9e9cdb46dc09a2f73abbd9934

# Docker's apt repository and its signing key's fingerprint.
DOCKER_APT_URL=https://download.docker.com/linux/ubuntu
DOCKER_GPG_FPR=9DC858229FC7DD38854AE2D88D81803C0EBFCD88
DOCKER_PACKAGES="docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin"

# The images, as crates/sentient-installer-core/src/distro.rs pins them
# (PG_IMAGE_STOCK, PG_IMAGE_TIMESCALE, CH_IMAGE); SENTIENT's is added from the
# release version (dockerhub.rs SENTIENT_IMAGE). The gateway on Windows is a
# service (gateway/SentientGatewaySetup-*.exe), so its image is not carried.
APP_IMAGES="postgres:18-alpine timescale/timescaledb:2.26.1-pg18 clickhouse/clickhouse-server:24.8-alpine"
SENTIENT_REPO=quakestring/sentient

GATEWAY_REPO=QuakeString/sentient-release
MIN_WINDOWS_BUILD=19044   # Windows 10 21H2

# ---------------------------------------------------------------------------
die() { echo "ERROR: $*" >&2; exit 1; }
log() { echo "== $*"; }

spm_exe= spm_version= spm_sha= release= gateway=latest key= pub="$root/packaging/release-signing.pub"
work=/mnt/data_1/INVENIA/spm-wip/offline-build out= gh_conf="$HOME/.config/sentient-release/github.curl" keep_staging=false
while [ $# -gt 0 ]; do
  case "$1" in
    --spm-exe) spm_exe=$2; shift 2 ;;
    --spm-version) spm_version=$2; shift 2 ;;
    --spm-sha256) spm_sha=$2; shift 2 ;;
    --sentient-release) release=$2; shift 2 ;;
    --gateway) gateway=$2; shift 2 ;;
    --key) key=$2; shift 2 ;;
    --pub) pub=$2; shift 2 ;;
    --work) work=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --github-curl-config) gh_conf=$2; shift 2 ;;
    --chrome-sha256) CHROME_MSI_SHA256=$2; shift 2 ;;
    --keep-staging) keep_staging=true; shift ;;
    -h|--help) sed -n '2,60p' "$0"; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done
[ -n "$spm_exe" ] || die "--spm-exe is required"
[ -n "$release" ] || die "--sentient-release is required"
[ -n "$key" ] || die "--key is required"
[ -f "$spm_exe" ] || die "$spm_exe: no such file"
[ -f "$release/manifest.json" ] || die "$release has no manifest.json"
[ -r "$key" ] || die "$key: cannot read the signing key"
[ -r "$pub" ] || die "$pub: cannot read the public key"
mode=$(stat -c '%a' "$key")
case "$mode" in 600|400) ;; *) die "$key is mode $mode: make it 0600 first" ;; esac
out=${out:-$work/out}

for t in curl jq zstd zip docker sha256sum gpg stat df awk; do
  command -v "$t" >/dev/null || die "$t is not installed"
done
if command -v rsign >/dev/null; then signer=rsign
elif command -v minisign >/dev/null; then signer=minisign
else die "neither rsign (cargo install rsign2) nor minisign is on PATH"; fi

# Room: the cache (~5 GB), the zip (~4 GB) and docker's own pulls.
mkdir -p "$work" "$out"
free_gb=$(( $(df --output=avail -k "$work" | tail -1) / 1024 / 1024 ))
mem_gb=$(( $(awk '/^MemAvailable:/ {print $2}' /proc/meminfo) / 1024 / 1024 ))
[ "$free_gb" -ge 15 ] || die "only ${free_gb} GB free under $work (need 15 GB)"
[ "$mem_gb" -ge 6 ] || die "only ${mem_gb} GB of memory available (need 6 GB)"
log "work $work: ${free_gb} GB free, ${mem_gb} GB memory available"

cache=$work/cache
mkdir -p "$cache"/{spm,payload,gateway,wsl,browser,debs,images,meta}

sha_of() { sha256sum "$1" | awk '{print $1}'; }
size_of() { stat -c '%s' "$1"; }
human() { numfmt --to=si --suffix=B --format='%.1f' "$1"; }

# A file at $2 whose SHA-256 is $3: kept when it already matches, else
# downloaded from $1 (with $4… as extra curl arguments) and checked.
fetch() {
  local url=$1 dest=$2 want=$3; shift 3
  if [ -f "$dest" ] && [ "$(sha_of "$dest")" = "$want" ]; then
    echo "   reused $(basename "$dest")"; return 0
  fi
  echo "   downloading $url"
  local progress=-sS; [ -t 2 ] && progress=-#
  curl -fL $progress --retry 3 --retry-delay 5 -o "$dest.part" "$@" "$url" || die "download failed: $url"
  local got; got=$(sha_of "$dest.part")
  if [ "$got" != "$want" ]; then
    rm -f "$dest.part"
    die "SHA-256 MISMATCH for $url: expected $want, got $got"
  fi
  mv "$dest.part" "$dest"
}

# A local file copied into the cache once, checked against $3.
take() {
  local src=$1 dest=$2 want=$3
  if [ -f "$dest" ] && [ "$(sha_of "$dest")" = "$want" ]; then
    echo "   reused $(basename "$dest")"; return 0
  fi
  local got; got=$(sha_of "$src")
  [ "$got" = "$want" ] || die "SHA-256 MISMATCH for $src: expected $want, got $got"
  cp "$src" "$dest.part" && mv "$dest.part" "$dest"
  [ "$(sha_of "$dest")" = "$want" ] || die "copy of $src changed on the way"
}

# GitHub API: with the token from a 0600 curl config, never on argv.
gh_args=(-H "Accept: application/vnd.github+json")
if [ -f "$gh_conf" ]; then
  case "$(stat -c '%a' "$gh_conf")" in 600|400) gh_args+=(-K "$gh_conf"); log "GitHub: token from $gh_conf" ;;
    *) die "$gh_conf is not mode 0600" ;; esac
else
  log "GitHub: anonymous (no $gh_conf)"
fi
gh_api() { curl -fsS --retry 3 "${gh_args[@]}" "https://api.github.com/$1"; }

# ---------------------------------------------------------------------------
log "Platform Manager"
if [ -z "$spm_version" ]; then
  spm_version=$(basename "$spm_exe" | sed -n 's/^SENTIENT-Setup-manager-\(.*\)\.exe$/\1/p')
  [ -n "$spm_version" ] || die "cannot read the version from $(basename "$spm_exe"); pass --spm-version"
fi
if [ -z "$spm_sha" ]; then
  for sums in "$(dirname "$spm_exe")/SHA256SUMS" "$spm_exe.sha256"; do
    [ -f "$sums" ] || continue
    spm_sha=$(awk -v f="$(basename "$spm_exe")" '{n=$2; sub(/^\*/,"",n)} n==f || NF==1 {print $1; exit}' "$sums")
    [ -n "$spm_sha" ] && break
  done
  [ -n "$spm_sha" ] || die "no SHA-256 listed for $(basename "$spm_exe") beside it: pass --spm-sha256 (from the release that published it)"
fi
take "$spm_exe" "$cache/spm/SENTIENT-Setup.exe" "$spm_sha"

# ---------------------------------------------------------------------------
log "SENTIENT release $release"
if [ -f "$release/manifest.json.minisig" ]; then
  # The release manifest is signed with the release key: check it with the
  # repository's public key (not the pack's --pub, which may be a test key).
  if [ "$signer" = rsign ]; then rsign verify -q -p "$root/packaging/release-signing.pub" -x "$release/manifest.json.minisig" "$release/manifest.json" >/dev/null
  else minisign -V -q -p "$root/packaging/release-signing.pub" -x "$release/manifest.json.minisig" -m "$release/manifest.json" >/dev/null; fi \
    || die "$release/manifest.json does not verify with packaging/release-signing.pub"
  echo "   manifest.json signature OK"
else
  die "$release/manifest.json.minisig is missing"
fi
sentient_version=$(jq -r .version "$release/manifest.json")
build_version=$(jq -r '.build_version // empty' "$release/manifest.json")
notes=$(jq -r '.notes // ""' "$release/manifest.json")
released=$(jq -r '.released // ""' "$release/manifest.json")
man_sha() { jq -r --arg f "$1" '.artifacts[] | select(.file==$f) | .sha256' "$release/manifest.json"; }
pg_zip=$(jq -r '.artifacts[].file | select(test("^postgresql-.*-windows-x64-binaries\\.zip$"))' "$release/manifest.json")
tsdb_zip=$(jq -r '.artifacts[].file | select(test("^timescaledb-.*-windows-amd64\\.zip$"))' "$release/manifest.json")
[ -n "$pg_zip" ] && [ -n "$tsdb_zip" ] || die "manifest.json lists no PostgreSQL or TimescaleDB zip"
for f in sentient-payload.tar "$pg_zip" "$tsdb_zip" vc_redist.x64.exe; do
  want=$(man_sha "$f"); [ -n "$want" ] || die "manifest.json does not list $f"
  [ -f "$release/$f" ] || die "$release/$f is missing"
  take "$release/$f" "$cache/payload/$f" "$want"
done
pg_version=$(echo "$pg_zip" | sed -n 's/^postgresql-\(.*\)-windows-x64-binaries\.zip$/\1/p')
# The TimescaleDB zip has no version in its name: the one native.rs pins.
tsdb_version=$(sed -n 's/^pub const TSDB_VERSION: &str = "\(.*\)";/\1/p' "$root/crates/sentient-installer-core/src/native.rs")
printf '%s\n' "$sentient_version" >"$cache/payload/VERSION"
printf '%s\n' "$notes" >"$cache/payload/NOTES.md"

# ---------------------------------------------------------------------------
log "Gateway $gateway"
if [ "$gateway" = latest ]; then
  gateway=$(gh_api "repos/$GATEWAY_REPO/releases?per_page=100" \
    | jq -r '[.[] | select(.tag_name|startswith("gateway-")) | select(.prerelease|not) | select(any(.assets[]; .name|test("^SentientGatewaySetup-.*\\.exe$")))][0].tag_name | ltrimstr("gateway-")')
  [ -n "$gateway" ] && [ "$gateway" != null ] || die "no gateway release with a Windows installer"
  echo "   newest: $gateway"
fi
gw_exe="SentientGatewaySetup-$gateway.exe"
gw_rel=$(gh_api "repos/$GATEWAY_REPO/releases/tags/gateway-$gateway") || die "no release gateway-$gateway"
gw_url=$(jq -r --arg n "$gw_exe" '.assets[] | select(.name==$n) | .browser_download_url' <<<"$gw_rel")
gw_sum_url=$(jq -r --arg n "$gw_exe.sha256" '.assets[] | select(.name==$n) | .browser_download_url' <<<"$gw_rel")
gw_digest=$(jq -r --arg n "$gw_exe" '.assets[] | select(.name==$n) | .digest // empty' <<<"$gw_rel")
[ -n "$gw_url" ] && [ -n "$gw_sum_url" ] || die "gateway-$gateway has no $gw_exe and $gw_exe.sha256"
gw_sha=$(curl -fsSL --retry 3 "$gw_sum_url" | awk '{print $1; exit}')
[[ "$gw_sha" =~ ^[0-9a-f]{64}$ ]] || die "$gw_exe.sha256 is not a SHA-256"
if [ -n "$gw_digest" ] && [ "$gw_digest" != "sha256:$gw_sha" ]; then
  die "gateway $gw_exe.sha256 ($gw_sha) disagrees with GitHub's digest ($gw_digest)"
fi
fetch "$gw_url" "$cache/gateway/$gw_exe" "$gw_sha"

# ---------------------------------------------------------------------------
log "WSL $WSL_VERSION, its kernel, Ubuntu $UBUNTU_RELEASE ($ROOTFS_DATE)"
wsl_msi="wsl.${WSL_VERSION}.0.x64.msi"
rootfs="ubuntu-noble-wsl-amd64-${ROOTFS_DATE}.rootfs.tar.gz"
fetch "$WSL_MSI_URL" "$cache/wsl/$wsl_msi" "$WSL_MSI_SHA256"
fetch "$KERNEL_MSI_URL" "$cache/wsl/wsl_update_x64.msi" "$KERNEL_MSI_SHA256"
fetch "$ROOTFS_URL" "$cache/wsl/$rootfs" "$ROOTFS_SHA256"

log "Google Chrome Enterprise (the kiosk's browser)"
chrome_msi=googlechromestandaloneenterprise64.msi
[[ "$CHROME_MSI_SHA256" =~ ^[0-9a-f]{64}$ ]] || die "--chrome-sha256 is not a SHA-256"
echo "   (a SHA-256 mismatch here: Google has published a newer Chrome; check it, then pass --chrome-sha256)"
fetch "$CHROME_MSI_URL" "$cache/browser/$chrome_msi" "$CHROME_MSI_SHA256"

# ---------------------------------------------------------------------------
# Docker Engine's packages, resolved against that very root file system:
# it is imported as a throwaway local image, and apt in a --rm container of
# it downloads Docker's packages and whatever they need that the rootfs
# lacks (or has too old), with the Recommends the online install
# (get.docker.com) installs too. apt's simulated install gives the order.
# Then the packages are installed with no network in a fresh container of
# the same rootfs: a dependency missing from the pack fails the build here,
# not on an air-gapped computer.
log "Docker Engine packages for Ubuntu $UBUNTU_RELEASE"
rootfs_image="spm-offline-rootfs:${UBUNTU_CODENAME}-${ROOTFS_DATE}"
debs=$cache/debs/$UBUNTU_CODENAME-$ROOTFS_DATE
deb_list=$debs/order.tsv
if [ -f "$deb_list" ] && [ -f "$debs/ok" ] && awk -F'\t' '{print $3 "  " $1}' "$deb_list" | (cd "$debs" && sha256sum -c --quiet -) 2>/dev/null; then
  echo "   reused $(wc -l <"$deb_list") packages (installed without network before)"
else
  rm -rf "$debs"; mkdir -p "$debs/raw"
  if ! docker image inspect "$rootfs_image" >/dev/null 2>&1; then
    docker import "$cache/wsl/$rootfs" "$rootfs_image" >/dev/null
  fi
  gpgkey=$cache/meta/docker-archive-keyring.asc
  curl -fsSL --retry 3 "$DOCKER_APT_URL/gpg" -o "$gpgkey.part"
  gpg --show-keys --with-colons "$gpgkey.part" 2>/dev/null | grep -q "^fpr:::::::::$DOCKER_GPG_FPR:" \
    || { rm -f "$gpgkey.part"; die "Docker's apt key does not have the fingerprint $DOCKER_GPG_FPR"; }
  mv "$gpgkey.part" "$gpgkey"
  docker run --rm --network bridge \
    -v "$debs/raw:/out" -v "$gpgkey:/etc/apt/keyrings/docker.asc:ro" \
    -e PKGS="$DOCKER_PACKAGES" -e CODENAME="$UBUNTU_CODENAME" -e REPO="$DOCKER_APT_URL" -e HOST_UID="$(id -u)" \
    "$rootfs_image" /bin/bash -euo pipefail -c '
      export DEBIAN_FRONTEND=noninteractive
      echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] $REPO $CODENAME stable" >/etc/apt/sources.list.d/docker.list
      apt-get update -qq
      # Inst lines in the order apt would install them (dependencies first).
      apt-get install -s -y $PKGS | awk "/^Inst /{print \$2}" >/out/order.txt
      apt-get install -y -qq --download-only $PKGS >/dev/null
      for p in $(cat /out/order.txt); do
        v=$(apt-cache policy "$p" | awk "/Candidate:/{print \$2}")
        printf "%s\t%s\t%s\n" "$p" "$v" "$(dpkg --print-architecture)" >>/out/versions.tsv
      done
      cp /var/cache/apt/archives/*.deb /out/
      chown -R "$HOST_UID" /out'
  # NN-<pkg>_<version without epoch>_<arch>.deb, in install order.
  n=0
  while IFS=$'\t' read -r pkg ver _; do
    n=$((n + 1))
    f=$(find "$debs/raw" -maxdepth 1 -name "${pkg}_*.deb" -print -quit)
    [ -n "$f" ] || die "apt did not download $pkg"
    name=$(printf '%02d-%s' "$n" "$(basename "$f" | sed 's/%3a/_/')")
    mv "$f" "$debs/$name"
    printf '%s\t%s\t%s\t%s\n' "$name" "$pkg" "$(sha_of "$debs/$name")" "$ver"
  done <"$debs/raw/versions.tsv" >"$deb_list"   # file, package, sha256, version
  left=$(find "$debs/raw" -name '*.deb' | wc -l)
  [ "$left" -eq 0 ] || die "apt downloaded $left packages outside its install plan: $(ls "$debs/raw/")"
  rm -rf "$debs/raw"
  echo "   $(wc -l <"$deb_list") packages; installing them with no network to prove they are complete"
  docker run --rm --network none -v "$debs:/debs:ro" "$rootfs_image" /bin/bash -euo pipefail -c '
      export DEBIAN_FRONTEND=noninteractive
      dpkg -i $(cut -f1 /debs/order.tsv | sed "s|^|/debs/|") >/tmp/dpkg.log 2>&1 || { tail -40 /tmp/dpkg.log; exit 1; }
      dpkg --audit
      docker --version; containerd --version; docker compose version; docker buildx version' \
    || die "the Docker packages do not install offline on the rootfs"
  touch "$debs/ok"
fi
docker_version=$(awk -F'\t' '$2=="docker-ce" {print $4}' "$deb_list" | sed 's/^[0-9]*://; s/-.*//')

# ---------------------------------------------------------------------------
log "Images (linux/amd64, docker save | zstd -19)"
images="$SENTIENT_REPO:$sentient_version $APP_IMAGES"
img_rows=$cache/meta/images.tsv
: >"$img_rows.new"
for ref in $images; do
  name=$(echo "${ref##*/}" | tr ':' '-')
  case "$ref" in clickhouse/*) name=clickhouse-${ref##*:} ;; esac
  file=$cache/images/$name.tar.zst
  docker pull -q --platform linux/amd64 "$ref" >/dev/null || die "docker pull $ref failed"
  digest=$(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$ref" | grep "^${ref%:*}@" | head -1 | sed 's/.*@//')
  [ -n "$digest" ] || digest=$(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "$ref" | head -1 | sed 's/.*@//')
  [ -n "$digest" ] || die "$ref has no registry digest"
  prev=$(awk -F'\t' -v r="$ref" '$1==r {print $2 "\t" $3}' "$img_rows" 2>/dev/null || true)
  if [ -f "$file" ] && [ -n "$prev" ] && [ "${prev%%$'\t'*}" = "$digest" ] && [ "$(sha_of "$file")" = "${prev##*$'\t'}" ]; then
    echo "   reused $name.tar.zst ($digest)"
    sha=${prev##*$'\t'}
  else
    echo "   saving $ref ($digest)"
    docker save --platform linux/amd64 "$ref" | zstd -19 -T4 -q -o "$file.part" || die "docker save $ref failed"
    zstd -t -q "$file.part" || die "$file.part is not a valid zstd file"
    mv -f "$file.part" "$file"
    sha=$(sha_of "$file")
  fi
  printf '%s\t%s\t%s\t%s\n' "$ref" "$digest" "$sha" "$name.tar.zst" >>"$img_rows.new"
done
mv "$img_rows.new" "$img_rows"

# ---------------------------------------------------------------------------
pack_name="SPM-offline-installer-${spm_version}-SENTIENT-${sentient_version}-windows-x64"
stage=$work/staging/$pack_name
log "Staging $stage (hard links into the cache)"
rm -rf "$work/staging"; mkdir -p "$stage"/{payload,gateway,wsl/docker-debs,browser,images}
rows=$work/staging/files.tsv   # path, kind, image_ref, image_digest
: >"$rows"
put() { # <source> <path in pack> <kind> [image_ref image_digest]
  ln "$1" "$stage/$2" 2>/dev/null || cp "$1" "$stage/$2"
  printf '%s\t%s\t%s\t%s\n' "$2" "$3" "${4:-}" "${5:-}" >>"$rows"
}
put "$cache/spm/SENTIENT-Setup.exe" SENTIENT-Setup.exe spm
cp "$root/app/assets/licence.txt" "$stage/EULA.txt"; printf 'EULA.txt\tother\t\t\n' >>"$rows"
cat >"$stage/README.txt" <<EOF
SENTIENT offline installer for Windows x64
Platform Manager $spm_version, SENTIENT $sentient_version, Sentient Gateway $gateway

Unpack this folder completely (to an NTFS or exFAT drive), then double-click
SENTIENT-Setup.exe. Setup checks the pack's signature and every file before it
changes anything, and installs without the internet.
To update an installation, use "Update from an offline pack" in the Platform
Manager and choose this folder.
EOF
printf 'README.txt\tother\t\t\n' >>"$rows"
put "$cache/payload/sentient-payload.tar" payload/sentient-payload.tar payload
put "$cache/payload/$pg_zip" "payload/$pg_zip" pg
put "$cache/payload/$tsdb_zip" "payload/$tsdb_zip" tsdb
put "$cache/payload/vc_redist.x64.exe" payload/vc_redist.x64.exe vcredist
put "$cache/payload/VERSION" payload/VERSION other
put "$cache/payload/NOTES.md" payload/NOTES.md other
put "$cache/gateway/$gw_exe" "gateway/$gw_exe" gateway
put "$cache/wsl/$wsl_msi" "wsl/$wsl_msi" wsl_msi
put "$cache/wsl/wsl_update_x64.msi" wsl/wsl_update_x64.msi kernel_msi
put "$cache/wsl/$rootfs" "wsl/$rootfs" rootfs
put "$cache/browser/$chrome_msi" "browser/$chrome_msi" chrome_msi
while IFS=$'\t' read -r f _ _ _; do put "$debs/$f" "wsl/docker-debs/$f" deb; done <"$deb_list"
while IFS=$'\t' read -r ref digest _ f; do put "$cache/images/$f" "images/$f" image "$ref" "$digest"; done <"$img_rows"

log "pack.json"
while IFS=$'\t' read -r path kind iref idig; do
  jq -n --arg path "$path" --arg sha "$(sha_of "$stage/$path")" --argjson size "$(size_of "$stage/$path")" \
        --arg kind "$kind" --arg iref "$iref" --arg idig "$idig" \
    '{path: $path, sha256: $sha, size: $size, kind: $kind}
     + (if $iref != "" then {image_ref: $iref} else {} end)
     + (if $idig != "" then {image_digest: $idig} else {} end)'
done <"$rows" | jq -s . >"$work/staging/files.json"
jq -n \
  --arg target windows-x64 --argjson build "$MIN_WINDOWS_BUILD" \
  --arg spm "$spm_version" --arg ver "$sentient_version" --arg bv "$build_version" --arg gw "$gateway" \
  --arg pg "$pg_version" --arg tsdb "$tsdb_version" --arg wsl "$WSL_VERSION" \
  --arg rootfs "$UBUNTU_RELEASE-$ROOTFS_DATE" --arg docker "$docker_version" \
  --arg created "$(date -u +%Y-%m-%d)" --arg notes "$notes" \
  --slurpfile files "$work/staging/files.json" \
  '{format: 1, target: $target, min_os: {windows_build: $build},
    spm_version: $spm, sentient_version: $ver}
   + (if $bv != "" then {build_version: $bv} else {} end)
   + {gateway_version: $gw,
      components: {postgresql: $pg, timescaledb: $tsdb, wsl: $wsl, ubuntu_rootfs: $rootfs, docker: $docker},
      created: $created, notes: $notes, files: $files[0]}' >"$stage/pack.json"

log "Signing pack.json ($signer, $(basename "$key"))"
comment="pack $spm_version SENTIENT $sentient_version windows-x64"
if [ "$signer" = rsign ]; then
  rsign sign -W -s "$key" -t "$comment" -x "$stage/pack.json.minisig" "$stage/pack.json" >/dev/null
  rsign verify -q -p "$pub" -x "$stage/pack.json.minisig" "$stage/pack.json" >/dev/null
else
  minisign -S -s "$key" -t "$comment" -x "$stage/pack.json.minisig" -m "$stage/pack.json" </dev/null >/dev/null
  minisign -V -q -p "$pub" -x "$stage/pack.json.minisig" -m "$stage/pack.json" >/dev/null
fi || die "pack.json.minisig does not verify with $pub"
if ! cmp -s "$pub" "$root/packaging/release-signing.pub"; then
  echo "   WARNING: signed with a key other than packaging/release-signing.pub: the Platform Manager refuses this pack. Test use only."
fi

log "Zip"
zipf=$out/$pack_name.zip
rm -f "$zipf" "$zipf.sha256" "$zipf.part"
# Already-compressed files are stored; the rest (the payload tar) deflated.
(cd "$work/staging" && zip -q -r -n .zst:.gz:.zip:.msi:.exe:.deb "$zipf.part" "$pack_name") || die "zip failed"
mv "$zipf.part" "$zipf"
(cd "$out" && sha256sum "$(basename "$zipf")" >"$(basename "$zipf").sha256")
if $keep_staging; then echo "   staging kept: $stage"; else rm -rf "$work/staging"; fi

# ---------------------------------------------------------------------------
log "Sizes"
sizes() {
  jq -r '.files | group_by(.kind)[] | [.[0].kind, (map(.size)|add), length] | @tsv' "$1"
}
tmpjson=$(mktemp); unzip -p "$zipf" "$pack_name/pack.json" >"$tmpjson"
printf '   %-11s %10s  %s\n' kind size files
while IFS=$'\t' read -r kind bytes count; do printf '   %-11s %10s  %s\n' "$kind" "$(human "$bytes")" "$count"; done < <(sizes "$tmpjson")
total=$(jq '[.files[].size]|add' "$tmpjson")
while IFS=$'\t' read -r ref _ _ f; do
  printf '   image %-45s %s\n' "$ref" "$(human "$(jq --arg p "images/$f" '.files[]|select(.path==$p).size' "$tmpjson")")"
done <"$img_rows"
rm -f "$tmpjson"
printf '   %-11s %10s\n' total "$(human "$total")"
printf '   %-11s %10s  %s\n' zip "$(human "$(size_of "$zipf")")" "$zipf"
echo "   sha256 $(cut -d' ' -f1 "$zipf.sha256")"
