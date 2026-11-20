#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Sign Windows programs and installers (.exe, .msi, .dll) with Authenticode
# from Linux: SHA-256, an RFC 3161 timestamp (so a signature stays valid after
# the certificate expires), then a verify against that certificate.
#
#   scripts/sign-windows.sh [--desc "SENTIENT Server"] [--check] <file>...
#     --check   only verify that each file is signed by the configured
#               certificate (no signing; exit 1 if one is not)
#
# The certificate (a .pfx with its key) and its password file:
#   SENTIENT_CODESIGN_PFX        default ~/.sentient-keys/codesign-selfsigned-2026-11/codesign.pfx
#   SENTIENT_CODESIGN_PASS_FILE  default <the .pfx's folder>/pfx-password.txt
#   SENTIENT_CODESIGN_TIMESTAMP_URL  default http://timestamp.digicert.com
# Both must be mode 0600. The key and its password are never on a command
# line and never printed. Today this is our self-signed INVENIA SYSTEMS
# certificate; a public one later only changes SENTIENT_CODESIGN_PFX (or this
# default), nothing else.
#
# osslsigncode from PATH, else from a throwaway container (debian:12 with the
# osslsigncode package; image st-osslsigncode:local, built once).
set -euo pipefail
desc=""
check=false
files=()
while [ $# -gt 0 ]; do
  case "$1" in
    --desc) desc=${2:?--desc needs a text}; shift 2 ;;
    --check) check=true; shift ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    *) files+=("$1"); shift ;;
  esac
done
[ ${#files[@]} -gt 0 ] || { echo "usage: $0 [--desc text] [--check] <file>..." >&2; exit 2; }

pfx=${SENTIENT_CODESIGN_PFX:-$HOME/.sentient-keys/codesign-selfsigned-2026-11/codesign.pfx}
passfile=${SENTIENT_CODESIGN_PASS_FILE:-$(dirname "$pfx")/pfx-password.txt}
tsa=${SENTIENT_CODESIGN_TIMESTAMP_URL:-http://timestamp.digicert.com}
[ -r "$pfx" ] || { echo "no code-signing certificate at $pfx (set SENTIENT_CODESIGN_PFX)" >&2; exit 2; }
[ -r "$passfile" ] || { echo "no password file at $passfile (set SENTIENT_CODESIGN_PASS_FILE)" >&2; exit 2; }
for f in "$pfx" "$passfile"; do
  mode=$(stat -c '%a' "$f")
  case "$mode" in 600|400) ;; *) echo "$f is mode $mode: make it 0600 first" >&2; exit 2 ;; esac
done
keydir=$(cd "$(dirname "$pfx")" && pwd)
pfxname=$(basename "$pfx")
passname=$(basename "$passfile")
[ "$(cd "$(dirname "$passfile")" && pwd)" = "$keydir" ] || { echo "the password file must sit next to the .pfx" >&2; exit 2; }

# The certificate alone (public), for the verify: our signer is its own root.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
openssl pkcs12 -in "$pfx" -passin "file:$passfile" -nokeys -clcerts 2>/dev/null | openssl x509 -out "$work/signer.pem"
cat /etc/ssl/certs/ca-certificates.crt "$work/signer.pem" > "$work/ca.pem" 2>/dev/null || cp "$work/signer.pem" "$work/ca.pem"
subject=$(openssl x509 -in "$work/signer.pem" -noout -subject -nameopt multiline | sed -n 's/^ *commonName *= *//p')

# osslsigncode, local or in a container. Every path it sees is under /k (the
# key folder, read-only), /w (this run's work folder) or /f (the file's folder).
if command -v osslsigncode >/dev/null 2>&1; then
  osc() { osslsigncode "$@"; }
  K=$keydir; W=$work
  map() { printf '%s' "$1"; }
else
  img=st-osslsigncode:local
  if ! docker image inspect "$img" >/dev/null 2>&1; then
    printf 'FROM debian:12\nRUN apt-get update && apt-get install -y --no-install-recommends osslsigncode ca-certificates && rm -rf /var/lib/apt/lists/*\n' |
      docker build -q -t "$img" - >/dev/null
  fi
  K=/k; W=/w
  osc() { docker run --rm --user "$(id -u):$(id -g)" -v "$keydir:/k:ro" -v "$work:/w" -v "$fdir:/f" "$img" osslsigncode "$@"; }
  map() { printf '/f/%s' "$(basename "$1")"; }
fi

rc=0
for f in "${files[@]}"; do
  [ -f "$f" ] || { echo "no file $f" >&2; exit 2; }
  fdir=$(cd "$(dirname "$f")" && pwd)
  name=$(basename "$f")
  in=$(map "$fdir/$name")
  if ! $check; then
    signed="$fdir/.$name.signed"
    rm -f "$signed"
    ok=false
    # A timestamp server can be briefly unavailable: three tries.
    for i in 1 2 3; do
      if osc sign -pkcs12 "$K/$pfxname" -readpass "$K/$passname" -h sha256 \
           ${desc:+-n "$desc"} -ts "$tsa" -in "$in" -out "$(map "$signed")" >/dev/null; then ok=true; break; fi
      sleep $((5 * i))
    done
    $ok || { rm -f "$signed"; echo "signing $f failed" >&2; exit 1; }
    chmod --reference="$f" "$signed"
    mv -f "$signed" "$f"
  fi
  if osc verify -CAfile "$W/ca.pem" -TSA-CAfile "$W/ca.pem" -in "$in" > "$work/verify.txt" 2>&1 &&
     grep -q "Subject: .*CN=${subject}" "$work/verify.txt"; then
    echo "signed by $subject: $f"
  else
    echo "NOT signed by $subject: $f" >&2
    $check || { sed -n '1,40p' "$work/verify.txt" >&2; exit 1; }
    rc=1
  fi
done
exit $rc
