#!/usr/bin/env bash
# Publish a SENTIENT release to github.com/QuakeString/sentient-releases, where
# SENTIENT Setup and the Platform Manager download it from.
#
#   scripts/publish-sentient-release.sh <version> <folder> [--notes "<text>"]
#
# <folder> holds the release files (any of):
#   sentient-payload.tar                          Windows x64
#   postgresql-18.6-1-windows-x64-binaries.zip    PostgreSQL for Windows
#   timescaledb-postgresql-18-windows-amd64.zip   TimescaleDB for it
#   vc_redist.x64.exe                             Visual C++ runtime
#   sentient-payload-linux-x86_64.tar             Linux x86_64
#   sentient-payload-linux-aarch64.tar            Linux arm64
# A manifest.json listing each with its size and SHA-256 is written and
# uploaded with them. The release becomes "latest". A version that is already
# published is refused: releases are immutable, publish a new version.
# Needs GITHUB_TOKEN (repo scope), or a github.com credential in git.
set -euo pipefail
version=${1:?version, e.g. 4.3.0.49}
dir=${2:?folder with the release files}
notes=""
[ "${3:-}" = "--notes" ] && notes=${4:-}
repo=QuakeString/sentient-releases
api=https://api.github.com/repos/$repo
token=${GITHUB_TOKEN:-$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | sed -n 's/^password=//p')}
[ -n "$token" ] || { echo "no GitHub token (set GITHUB_TOKEN)" >&2; exit 2; }
auth=(-H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json")

if curl -fs "${auth[@]}" "$api/releases/tags/$version" >/dev/null; then
  echo "$version is already published; releases are immutable." >&2; exit 1
fi

files=()
for f in sentient-payload.tar postgresql-18.6-1-windows-x64-binaries.zip timescaledb-postgresql-18-windows-amd64.zip \
         vc_redist.x64.exe sentient-payload-linux-x86_64.tar sentient-payload-linux-aarch64.tar; do
  [ -f "$dir/$f" ] && files+=("$f")
done
[ ${#files[@]} -gt 0 ] || { echo "no release files in $dir" >&2; exit 1; }

# manifest.json: what Setup reads to know what to download and how to check it.
python3 - "$version" "$dir" "$repo" "$notes" "${files[@]}" > "$dir/manifest.json" <<'PY'
import hashlib, json, os, sys, datetime
version, d, repo, notes, *files = sys.argv[1:]
def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()
arts = [{
    "file": f,
    "url": f"https://github.com/{repo}/releases/download/{version}/{f}",
    "sha256": sha(os.path.join(d, f)),
    "size": os.path.getsize(os.path.join(d, f)),
} for f in files]
print(json.dumps({"product": "SENTIENT", "version": version,
                  "released": datetime.date.today().isoformat(), "notes": notes,
                  "artifacts": arts}, indent=2))
PY
echo "manifest:"; cat "$dir/manifest.json"

body=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": "main", "name": "SENTIENT " + sys.argv[1], "body": sys.argv[2] or "SENTIENT " + sys.argv[1], "make_latest": "true"}))' "$version" "$notes")
release=$(curl -fsS "${auth[@]}" -X POST "$api/releases" -d "$body")
upload=$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
for f in "${files[@]}" manifest.json; do
  echo "uploading $f ($(du -h "$dir/$f" | cut -f1))"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/$f" "$upload?name=$f" >/dev/null
done
echo "published: https://github.com/$repo/releases/tag/$version"
