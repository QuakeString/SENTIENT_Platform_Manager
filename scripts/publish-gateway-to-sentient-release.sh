#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Publish a Sentient Gateway release on github.com/QuakeString/sentient-release
# (tag gateway-<version>), then refresh that repository's Downloads table.
#
#   scripts/publish-gateway-to-sentient-release.sh <version> <gateway-repo>
#
# Takes from <gateway-repo> (a sentient-gateway-rs checkout at that version):
#   target/installer/SentientGatewaySetup-<version>.exe   packaging/windows/build.sh
#   target/linux-packages/*                                packaging/linux/build.sh
# and adds the installer's .sha256 and a .zip of it, as earlier releases had.
# Notes: the version's CHANGELOG.md section (release-notes.py body --product
# gateway: no PR links, with the downloads). Never the repository's Latest,
# which must stay SENTIENT's (SENTIENT Setup reads it). Needs GITHUB_TOKEN
# (repo scope), or a github.com credential in git.
set -euo pipefail
version=${1:?usage: $0 <version> <gateway-repo>}
gw=$(cd "${2:?usage: $0 <version> <gateway-repo>}" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
dst=QuakeString/sentient-release
tag=gateway-$version
token=${GITHUB_TOKEN:-$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | sed -n 's/^password=//p')}
[ -n "$token" ] || { echo "no GitHub token (set GITHUB_TOKEN)" >&2; exit 2; }
# The token goes to curl in a private (0600) config file, never on its
# command line, where other users could read it in the process list.
authcfg=$(umask 077; mktemp)
trap 'rm -f "$authcfg"' EXIT
printf 'header = "Authorization: Bearer %s"\nheader = "Accept: application/vnd.github+json"\n' "$token" > "$authcfg"
auth=(-K "$authcfg")

python3 "$here/release-notes.py" check "$gw/CHANGELOG.md" "$version"
if curl -fs "${auth[@]}" "https://api.github.com/repos/$dst/releases/tags/$tag" >/dev/null; then
  echo "$tag is already published on $dst; releases are immutable." >&2; exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work" "$authcfg"' EXIT
exe="$gw/target/installer/SentientGatewaySetup-$version.exe"
[ -f "$exe" ] || { echo "missing $exe (packaging/windows/build.sh)" >&2; exit 1; }
# Code-signed (packaging/windows/build.sh signs it) whenever this machine has
# the code-signing certificate; SENTIENT_ALLOW_UNSIGNED=1 publishes it anyway.
pfx=${SENTIENT_CODESIGN_PFX:-$HOME/.sentient-keys/codesign-selfsigned-2026-11/codesign.pfx}
if [ -f "$pfx" ] && [ "${SENTIENT_ALLOW_UNSIGNED:-}" != 1 ]; then
  "$here/sign-windows.sh" --check "$exe" ||
    { echo "$exe is not code-signed: sign it with scripts/sign-windows.sh --desc \"SENTIENT Gateway Setup\" first" >&2; exit 1; }
fi
cp "$exe" "$work/"
( cd "$work" && sha256sum "SentientGatewaySetup-$version.exe" > "SentientGatewaySetup-$version.exe.sha256" \
  && python3 -c 'import sys, zipfile; z = zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_DEFLATED); z.write(sys.argv[2]); z.close()' \
       "SentientGatewaySetup-$version.zip" "SentientGatewaySetup-$version.exe" )
n=0
for f in "$gw"/target/linux-packages/*; do
  case "$(basename "$f")" in *"$version"*|SHA256SUMS) cp "$f" "$work/"; n=$((n + 1)) ;; esac
done
[ "$n" -ge 2 ] || { echo "no Linux packages for $version in $gw/target/linux-packages (packaging/linux/build.sh)" >&2; exit 1; }

body=$(python3 "$here/release-notes.py" body "$gw/CHANGELOG.md" "$version" --product gateway)
payload=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": "main", "name": "Sentient Gateway " + sys.argv[2], "body": sys.argv[3], "prerelease": False, "make_latest": "false"}))' "$tag" "$version" "$body")
release=$(curl -fsS "${auth[@]}" -X POST "https://api.github.com/repos/$dst/releases" -d "$payload")
upload=$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
for f in "$work"/*; do
  echo "uploading $(basename "$f") ($(du -h "$f" | cut -f1))"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$f" "$upload?name=$(basename "$f")" >/dev/null
done
echo "published: https://github.com/$dst/releases/tag/$tag"

latest=$(curl -fsS "${auth[@]}" "https://api.github.com/repos/$dst/releases/latest" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')
case "$latest" in
  [0-9]*) echo "latest is still SENTIENT $latest" ;;
  *) echo "WARNING: $dst's latest release is now $latest, not a SENTIENT version" >&2; exit 1 ;;
esac
"$here/update-sentient-release-readme.sh"
