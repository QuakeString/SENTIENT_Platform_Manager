#!/usr/bin/env bash
# Publish a SENTIENT release to github.com/QuakeString/sentient-release, where
# SENTIENT Setup and the Platform Manager download it from.
#
#   scripts/publish-sentient-release.sh <version> <folder> [--notes "<text>"] [--docker-image <image>]
#   scripts/publish-sentient-release.sh <version> <folder> --add [--docker-image <image>]
#     (add files for another system to a published release: only files it
#      does not have yet, never replacing one; its manifest is extended)
#
# <folder> holds the release files (any of):
#   sentient-payload.tar                          Windows x64
#   postgresql-18.6-1-windows-x64-binaries.zip    PostgreSQL for Windows
#   timescaledb-postgresql-18-windows-amd64.zip   TimescaleDB for it
#   vc_redist.x64.exe                             Visual C++ runtime
#   sentient-payload-linux-x86_64.tar             Linux x86_64
#   sentient-payload-linux-aarch64.tar            Linux arm64
# A manifest.json listing each with its size and SHA-256 is written and
# uploaded with them. It also lists, under "binaries", the SHA-256 of the
# server program inside each payload (bin/sentient-server[.exe]) and, with
# --docker-image, inside that image: the hash a running SENTIENT reports, which
# the license server registers as a known build. The release becomes "latest". A version that is already
# published is refused: releases are immutable, publish a new version.
# Needs GITHUB_TOKEN (repo scope), or a github.com credential in git.
set -euo pipefail
version=${1:?version, e.g. 4.3.0.49}
dir=${2:?folder with the release files}
notes=""
add=false
docker_image=""
shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --notes) notes=${2:-}; shift 2 ;;
    --add) add=true; shift ;;
    --docker-image) docker_image=${2:?--docker-image needs an image}; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
repo=QuakeString/sentient-release
api=https://api.github.com/repos/$repo
token=${GITHUB_TOKEN:-$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | sed -n 's/^password=//p')}
[ -n "$token" ] || { echo "no GitHub token (set GITHUB_TOKEN)" >&2; exit 2; }
auth=(-H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json")

existing=$(curl -fs "${auth[@]}" "$api/releases/tags/$version" || true)
if [ -n "$existing" ] && ! $add; then
  echo "$version is already published; releases are immutable (use --add for another system's files)." >&2; exit 1
fi
if [ -z "$existing" ] && $add; then
  echo "$version is not published yet; publish it first." >&2; exit 1
fi

files=()
for f in sentient-payload.tar postgresql-18.6-1-windows-x64-binaries.zip timescaledb-postgresql-18-windows-amd64.zip \
         vc_redist.x64.exe sentient-payload-linux-x86_64.tar sentient-payload-linux-aarch64.tar; do
  [ -f "$dir/$f" ] && files+=("$f")
done
[ ${#files[@]} -gt 0 ] || { echo "no release files in $dir" >&2; exit 1; }

# SHA-256 of the server program in each payload and the Docker image, as
# JSON lines {"platform", "sha256"}. Read straight from the tar, nothing is
# unpacked to disk.
binaries=$(python3 - "$dir" <<'PY'
import hashlib, json, os, sys, tarfile
d = sys.argv[1]
payloads = {
    "sentient-payload.tar": ("windows-x86_64", "bin/sentient-server.exe"),
    "sentient-payload-linux-x86_64.tar": ("linux-x86_64", "bin/sentient-server"),
    "sentient-payload-linux-aarch64.tar": ("linux-aarch64", "bin/sentient-server"),
}
for name, (platform, member) in payloads.items():
    path = os.path.join(d, name)
    if not os.path.isfile(path):
        continue
    with tarfile.open(path) as tar:
        entry = next((m for m in tar if m.isfile() and m.name.lstrip("./") == member), None)
        if entry is None:
            sys.exit(f"{name}: no {member} inside")
        h = hashlib.sha256()
        f = tar.extractfile(entry)
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    print(json.dumps({"platform": platform, "sha256": h.hexdigest()}))
PY
)
if [ -n "$docker_image" ]; then
  # A stopped throwaway container just to copy the binary out; removed at once.
  arch=$(docker image inspect --format '{{.Architecture}}' "$docker_image")
  cid=$(docker create "$docker_image")
  tmp=$(mktemp)
  docker cp "$cid:/usr/share/sentient/bin/sentient-server" "$tmp" >/dev/null && docker rm "$cid" >/dev/null || { docker rm "$cid" >/dev/null; rm -f "$tmp"; exit 1; }
  case "$arch" in amd64) arch=x86_64 ;; arm64) arch=aarch64 ;; esac
  binaries+=$'\n'$(python3 -c 'import hashlib,json,sys; print(json.dumps({"platform": "docker-linux-" + sys.argv[2], "sha256": hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest()}))' "$tmp" "$arch")
  rm -f "$tmp"
fi
printf '%s\n' "$binaries" | sed '/^$/d' > "$dir/binaries.jsonl"

if $add; then
  have=$(printf '%s' "$existing" | python3 -c 'import json,sys; print(" ".join(a["name"] for a in json.load(sys.stdin)["assets"]))')
  new=()
  for f in "${files[@]}"; do
    case " $have " in *" $f "*) echo "skipping $f: the release already has it" ;; *) new+=("$f") ;; esac
  done
  [ ${#new[@]} -gt 0 ] || { echo "nothing to add" >&2; exit 1; }
  curl -fsSL "https://github.com/$repo/releases/download/$version/manifest.json" -o "$dir/manifest.old.json"
  python3 - "$version" "$dir" "$repo" "${new[@]}" > "$dir/manifest.json" <<'PY'
import hashlib, json, os, sys
version, d, repo, *files = sys.argv[1:]
m = json.load(open(os.path.join(d, "manifest.old.json")))
def sha(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()
for f in files:
    m["artifacts"].append({"file": f, "url": f"https://github.com/{repo}/releases/download/{version}/{f}",
                           "sha256": sha(os.path.join(d, f)), "size": os.path.getsize(os.path.join(d, f))})
known = {b["sha256"] for b in m.get("binaries", [])}
for line in open(os.path.join(d, "binaries.jsonl")):
    b = json.loads(line)
    if b["sha256"] not in known:
        m.setdefault("binaries", []).append(b)
        known.add(b["sha256"])
print(json.dumps(m, indent=2))
PY
  upload=$(printf '%s' "$existing" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
  for f in "${new[@]}"; do
    echo "uploading $f ($(du -h "$dir/$f" | cut -f1))"
    curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/$f" "$upload?name=$f" >/dev/null
  done
  # The manifest last: until it lists the new files nobody downloads them.
  old_id=$(printf '%s' "$existing" | python3 -c 'import json,sys; print(next(a["id"] for a in json.load(sys.stdin)["assets"] if a["name"]=="manifest.json"))')
  curl -fsS "${auth[@]}" -X DELETE "$api/releases/assets/$old_id"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/manifest.json" "$upload?name=manifest.json" >/dev/null
  rm -f "$dir/manifest.old.json" "$dir/binaries.jsonl"
  echo "added to https://github.com/$repo/releases/tag/$version: ${new[*]}"
  exit 0
fi

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
binaries = [json.loads(line) for line in open(os.path.join(d, "binaries.jsonl"))]
print(json.dumps({"product": "SENTIENT", "version": version,
                  "released": datetime.date.today().isoformat(), "notes": notes,
                  "artifacts": arts, "binaries": binaries}, indent=2))
PY
rm -f "$dir/binaries.jsonl"
echo "manifest:"; cat "$dir/manifest.json"

body=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": "main", "name": "SENTIENT " + sys.argv[1], "body": sys.argv[2] or "SENTIENT " + sys.argv[1], "make_latest": "true"}))' "$version" "$notes")
release=$(curl -fsS "${auth[@]}" -X POST "$api/releases" -d "$body")
upload=$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
for f in "${files[@]}" manifest.json; do
  echo "uploading $f ($(du -h "$dir/$f" | cut -f1))"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/$f" "$upload?name=$f" >/dev/null
done
echo "published: https://github.com/$repo/releases/tag/$version"
