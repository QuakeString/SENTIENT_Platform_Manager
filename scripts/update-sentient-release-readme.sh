#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Refresh the Downloads table in github.com/QuakeString/sentient-release's
# README.md: the newest SENTIENT, Platform Manager, gateway and mobile app
# releases, each linked to its release page. Run after publishing any of
# them (publish-spm-to-sentient-release.sh does). Arguments are ignored: the
# table is rebuilt from the release list. Needs GITHUB_TOKEN (repo scope),
# or a github.com credential in git.
set -euo pipefail
repo=QuakeString/sentient-release
token=${GITHUB_TOKEN:-$(printf 'protocol=https\nhost=github.com\n\n' | git credential fill 2>/dev/null | sed -n 's/^password=//p')}
[ -n "$token" ] || { echo "no GitHub token (set GITHUB_TOKEN)" >&2; exit 2; }
TOKEN=$token REPO=$repo python3 - <<'PY'
import base64, json, os, re, urllib.request

repo, token = os.environ["REPO"], os.environ["TOKEN"]
def api(path, method="GET", data=None):
    req = urllib.request.Request(f"https://api.github.com/repos/{repo}{path}", method=method,
                                 data=json.dumps(data).encode() if data is not None else None,
                                 headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req) as r:
        return json.load(r)

releases = [r for r in api("/releases?per_page=100") if not r["draft"]]
latest = api("/releases/latest")
def newest(prefix):
    # The API lists newest first.
    return next((r for r in releases if r["tag_name"].startswith(prefix)), None)

rows = []
def row(what, rel, about):
    if rel:
        rows.append(f"| {what} | [{rel['name'] or rel['tag_name']}]({rel['html_url']}) | {about} |")
row("SENTIENT", latest, "The platform for Windows and Linux; installed and updated by the Platform Manager. Docker: `quakestring/sentient`.")
row("SENTIENT Platform Manager", newest("spm-v"), "Installs, updates, backs up and manages SENTIENT (Windows installer, Linux binary and Arch package).")
row("SENTIENT Gateway", newest("gateway-"), "Connects field devices and PLCs to SENTIENT (Windows installer, Linux packages).")
row("SENTIENT mobile app", newest("SENTIENT-mobile-app-"), "Android app for SENTIENT dashboards and alarms.")
table = ("<!-- downloads:start -->\n## Downloads\n\n| Product | Latest | What it is |\n|---|---|---|\n"
         + "\n".join(rows) + "\n\nEvery release page lists its files with their SHA-256.\n<!-- downloads:end -->")

cur = api("/contents/README.md")
text = base64.b64decode(cur["content"]).decode()
if "<!-- downloads:start -->" in text:
    new = re.sub(r"<!-- downloads:start -->.*?<!-- downloads:end -->", lambda m: table, text, flags=re.S)
else:
    # After the opening paragraph.
    head, sep, rest = text.partition("\n\n")
    head2, sep2, rest2 = rest.partition("\n\n")
    new = head + sep + head2 + "\n\n" + table + "\n\n" + rest2
if new == text:
    print("README.md: Downloads table already current")
else:
    api("/contents/README.md", "PUT", {"message": "README: Downloads table", "content": base64.b64encode(new.encode()).decode(),
                                       "sha": cur["sha"], "branch": "main"})
    print("README.md: Downloads table updated")
PY
