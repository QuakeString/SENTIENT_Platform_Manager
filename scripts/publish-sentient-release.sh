#!/usr/bin/env bash
# Publish a SENTIENT release to github.com/QuakeString/sentient-release, where
# SENTIENT Setup and the Platform Manager download it from.
#
#   scripts/publish-sentient-release.sh <version> <folder> --changelog <SENTIENT/CHANGELOG.md> [--docker-image <image>]
#   scripts/publish-sentient-release.sh <version> <folder> [--notes "<text>"] [--docker-image <image>]
#     --changelog: the release notes are that version's section of SENTIENT's
#     CHANGELOG.md, without PR links (scripts/release-notes.py); the section
#     must exist, with a summary, or nothing is published. The summary is the
#     manifest's "notes". --notes "<text>" is the old one-paragraph form.
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
# the license server registers as a known build. Each Linux payload's artifact
# entry also gets "min_glibc": the highest non-weak GLIBC_x.y any ELF program
# in its bin/ needs (scripts/elf-min-glibc.sh), the oldest glibc it runs on.
# The release becomes "latest". A version that is already
# published is refused: releases are immutable, publish a new version.
# Needs GITHUB_TOKEN (repo scope), or a github.com credential in git.
set -euo pipefail
version=${1:?version, e.g. 4.3.0.49}
dir=${2:?folder with the release files}
notes=""
changelog=""
add=false
docker_image=""
shift 2
while [ $# -gt 0 ]; do
  case "$1" in
    --notes) notes=${2:-}; shift 2 ;;
    --changelog) changelog=${2:?--changelog needs the CHANGELOG.md path}; shift 2 ;;
    --add) add=true; shift ;;
    --docker-image) docker_image=${2:?--docker-image needs an image}; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
here=$(cd "$(dirname "$0")" && pwd)
body_notes=""
if [ -n "$changelog" ]; then
  python3 "$here/release-notes.py" check "$changelog" "$version" || exit 1
  notes=$(python3 "$here/release-notes.py" extract "$changelog" "$version" --public --summary)
  body_notes=$(python3 "$here/release-notes.py" extract "$changelog" "$version" --public)
elif [ -z "$notes" ] && ! $add; then
  echo "warning: no --changelog: the release gets no proper notes" >&2
fi
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
  # Every architecture the image is published for (a multi-arch tag lists
  # several; a local-only image has just its own). For each: a stopped
  # throwaway container of that platform, just to copy the binary out.
  arches=$(docker buildx imagetools inspect --raw "$docker_image" 2>/dev/null | python3 -c 'import json,sys
m = json.load(sys.stdin)
print(" ".join(sorted({x["platform"]["architecture"] for x in m.get("manifests", []) if x.get("platform", {}).get("os") == "linux"})))' 2>/dev/null || true)
  [ -n "$arches" ] || arches=$(docker image inspect --format '{{.Architecture}}' "$docker_image")
  for arch in $arches; do
    docker pull -q --platform "linux/$arch" "$docker_image" >/dev/null 2>&1 || true
    cid=$(docker create --platform "linux/$arch" "$docker_image")
    tmp=$(mktemp)
    docker cp "$cid:/usr/share/sentient/bin/sentient-server" "$tmp" >/dev/null && docker rm "$cid" >/dev/null || { docker rm "$cid" >/dev/null; rm -f "$tmp"; exit 1; }
    case "$arch" in amd64) name=x86_64 ;; arm64) name=aarch64 ;; *) name=$arch ;; esac
    binaries+=$'\n'$(python3 -c 'import hashlib,json,sys; print(json.dumps({"platform": "docker-linux-" + sys.argv[2], "sha256": hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest()}))' "$tmp" "$name")
    rm -f "$tmp"
  done
fi
printf '%s\n' "$binaries" | sed '/^$/d' > "$dir/binaries.jsonl"

# min_glibc of each Linux payload, as JSON lines {"file", "min_glibc"}. Only
# bin/ is unpacked, into a temporary folder next to the release files (not
# /tmp: the programs are hundreds of MB).
: > "$dir/glibc.jsonl"
for f in sentient-payload-linux-x86_64.tar sentient-payload-linux-aarch64.tar; do
  [ -f "$dir/$f" ] || continue
  t=$(mktemp -d -p "$dir" .glibc.XXXXXX)
  members=$(tar -tf "$dir/$f" | grep -E '^(\./)?bin/[^/]+$' || true)
  [ -n "$members" ] || { rm -rf "$t"; echo "$f: nothing in bin/" >&2; exit 1; }
  # shellcheck disable=SC2086
  tar -xf "$dir/$f" -C "$t" $members
  v=$("$here/elf-min-glibc.sh" "$t"/bin/* || true)
  rm -rf "$t"
  [ -n "$v" ] || { echo "$f: could not read the glibc version its programs need (readelf installed?)" >&2; exit 1; }
  echo "$f: needs glibc $v"
  python3 -c 'import json,sys; print(json.dumps({"file": sys.argv[1], "min_glibc": sys.argv[2]}))' "$f" "$v" >> "$dir/glibc.jsonl"
done

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
glibc = {g["file"]: g["min_glibc"] for g in map(json.loads, open(os.path.join(d, "glibc.jsonl")))}
for f in files:
    a = {"file": f, "url": f"https://github.com/{repo}/releases/download/{version}/{f}",
         "sha256": sha(os.path.join(d, f)), "size": os.path.getsize(os.path.join(d, f))}
    if f in glibc:
        a["min_glibc"] = glibc[f]
    m["artifacts"].append(a)
# Top level too: what SPM 0.9.3+ reads (Manifest.min_glibc) is the x86_64
# Linux payload's.
if "sentient-payload-linux-x86_64.tar" in glibc:
    m["min_glibc"] = glibc["sentient-payload-linux-x86_64.tar"]
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
  # Its signature goes with it (the old one would not match the new manifest).
  "$here/sign-release.sh" "$dir/manifest.json"
  for asset in manifest.json manifest.json.minisig; do
    old_id=$(printf '%s' "$existing" | python3 -c 'import json,sys; print(next((a["id"] for a in json.load(sys.stdin)["assets"] if a["name"]==sys.argv[1]), ""))' "$asset")
    [ -n "$old_id" ] && curl -fsS "${auth[@]}" -X DELETE "$api/releases/assets/$old_id"
  done
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/manifest.json.minisig" "$upload?name=manifest.json.minisig" >/dev/null
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/manifest.json" "$upload?name=manifest.json" >/dev/null
  rm -f "$dir/manifest.old.json" "$dir/binaries.jsonl" "$dir/glibc.jsonl"
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
glibc = {g["file"]: g["min_glibc"] for g in map(json.loads, open(os.path.join(d, "glibc.jsonl")))}
for a in arts:
    if a["file"] in glibc:
        a["min_glibc"] = glibc[a["file"]]
binaries = [json.loads(line) for line in open(os.path.join(d, "binaries.jsonl"))]
out = {"product": "SENTIENT", "version": version,
       "released": datetime.date.today().isoformat(), "notes": notes,
       "artifacts": arts, "binaries": binaries}
# Top level too: what SPM 0.9.3+ reads (Manifest.min_glibc) is the x86_64
# Linux payload's.
if "sentient-payload-linux-x86_64.tar" in glibc:
    out["min_glibc"] = glibc["sentient-payload-linux-x86_64.tar"]
print(json.dumps(out, indent=2))
PY
rm -f "$dir/binaries.jsonl" "$dir/glibc.jsonl"
echo "manifest:"; cat "$dir/manifest.json"
# Signed with the release key (scripts/sign-release.sh): the Platform Manager
# checks it before it downloads anything the manifest lists.
"$here/sign-release.sh" "$dir/manifest.json"

# The release page: the changelog section, then what to download.
if [ -n "$body_notes" ]; then
  downloads="### Downloads"$'\n'
  for f in "${files[@]}"; do
    case "$f" in
      sentient-payload.tar) downloads+="- Windows x64: \`$f\`, with PostgreSQL 18.6, TimescaleDB and the VC++ runtime"$'\n' ;;
      sentient-payload-linux-x86_64.tar) downloads+="- Linux x86_64: \`$f\`"$'\n' ;;
      sentient-payload-linux-aarch64.tar) downloads+="- Linux arm64: \`$f\`"$'\n' ;;
    esac
  done
  [ -n "$docker_image" ] && downloads+="- Docker: \`$docker_image\`"$'\n'
  downloads+="- \`manifest.json\` lists every file with its SHA-256, and \`manifest.json.minisig\` is its signature (minisign, the key in the Platform Manager's packaging/release-signing.pub). Install and update with the SENTIENT Platform Manager."
  body_notes+=$'\n\n'"$downloads"
fi
body=$(python3 -c 'import json,sys; print(json.dumps({"tag_name": sys.argv[1], "target_commitish": "main", "name": "SENTIENT " + sys.argv[1], "body": sys.argv[3] or sys.argv[2] or "SENTIENT " + sys.argv[1], "make_latest": "true"}))' "$version" "$notes" "$body_notes")
release=$(curl -fsS "${auth[@]}" -X POST "$api/releases" -d "$body")
upload=$(printf '%s' "$release" | python3 -c 'import json,sys; print(json.load(sys.stdin)["upload_url"].split("{")[0])')
for f in "${files[@]}" manifest.json.minisig manifest.json; do
  echo "uploading $f ($(du -h "$dir/$f" | cut -f1))"
  curl -fsS "${auth[@]}" -H "Content-Type: application/octet-stream" --data-binary "@$dir/$f" "$upload?name=$f" >/dev/null
done
echo "published: https://github.com/$repo/releases/tag/$version"
