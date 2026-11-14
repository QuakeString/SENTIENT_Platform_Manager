#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Publish a Platform Manager release on the public download repository
# github.com/QuakeString/sentient-release as well, beside SENTIENT, the
# gateway and the mobile app, and point its README's Downloads table at it.
#
#   scripts/publish-spm-to-sentient-release.sh <version>      e.g. 0.8.0-rc10
#
# The installers are taken from this repository's release v<version> (built
# by .github/workflows/build-installers.yml), so run it once that release is
# complete. Tag spm-v<version>, title "SENTIENT Platform Manager <version>",
# notes from CHANGELOG.md (public: no PR links) with each file's SHA-256.
#
# It never becomes that repository's "Latest" release: SENTIENT Setup and the
# Platform Manager read releases/latest/download/manifest.json there, which
# must stay SENTIENT's. A release candidate is also a pre-release.
# Needs GITHUB_TOKEN (repo scope), or a github.com credential in git.
set -euo pipefail
version=${1:?usage: $0 <version>}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
src=QuakeString/SENTIENT_Platform_Manager
dst=QuakeString/sentient-release
tag=spm-v$version
token=${GITHUB_TOKEN:-$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | sed -n 's/^password=//p')}
[ -n "$token" ] || { echo "no GitHub token (set GITHUB_TOKEN)" >&2; exit 2; }
auth=(-H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json")

python3 "$here/release-notes.py" check "$root/CHANGELOG.md" "$version"
if curl -fs "${auth[@]}" "https://api.github.com/repos/$dst/releases/tags/$tag" >/dev/null; then
  echo "$tag is already published on $dst; releases are immutable." >&2; exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsS "${auth[@]}" "https://api.github.com/repos/$src/releases/tags/v$version" > "$work/src.json"
python3 - "$work/src.json" > "$work/assets.tsv" <<'PY'
import json, sys
for a in json.load(open(sys.argv[1]))["assets"]:
    print(f'{a["name"]}\t{a["url"]}')
PY
[ -s "$work/assets.tsv" ] || { echo "v$version on $src has no files yet" >&2; exit 1; }
mkdir "$work/files"
while IFS=$'\t' read -r name url; do
  echo "downloading $name"
  curl -fsSL "${auth[@]}" -H "Accept: application/octet-stream" -o "$work/files/$name" "$url"
done < "$work/assets.tsv"

notes=$(python3 "$here/release-notes.py" extract "$root/CHANGELOG.md" "$version" --public)
table=$'| File | Size | SHA-256 |\n|---|---|---|\n'
for f in "$work"/files/*; do
  n=$(basename "$f")
  table+="| \`$n\` | $(du -h "$f" | cut -f1) | \`$(sha256sum "$f" | cut -d' ' -f1)\` |"$'\n'
done
body="$notes"$'\n\n'"### Downloads"$'\n\n'"Windows: run \`SENTIENT-Setup-manager-*.exe\`. Linux: \`SENTIENT-Setup-manager-*-linux-x86_64\`, or the Arch package."$'\n\n'"$table"
pre=false; case "$version" in *-rc*|*nightly*) pre=true ;; esac
payload=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": "main", "name": "SENTIENT Platform Manager " + sys.argv[2], "body": sys.argv[3], "prerelease": sys.argv[4] == "true", "make_latest": "false"}))' "$tag" "$version" "$body" "$pre")
release=$(curl -fsS "${auth[@]}" -X POST "https://api.github.com/repos/$dst/releases" -d "$payload")
upload=$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
for f in "$work"/files/*; do
  n=$(basename "$f")
  echo "uploading $n"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$f" "$upload?name=$n" >/dev/null
done
echo "published: https://github.com/$dst/releases/tag/$tag"

# Latest must still be SENTIENT's.
latest=$(curl -fsS "${auth[@]}" "https://api.github.com/repos/$dst/releases/latest" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')
case "$latest" in
  [0-9]*) echo "latest is still SENTIENT $latest" ;;
  *) echo "WARNING: $dst's latest release is now $latest, not a SENTIENT version" >&2; exit 1 ;;
esac

"$here/update-sentient-release-readme.sh" spm "$tag"
