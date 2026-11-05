#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
"""Release notes from a repository's CHANGELOG.md (Keep a Changelog format).

  release-notes.py extract <CHANGELOG.md> <version> [--public] [--summary]
      Print the notes of <version>: its section without the heading.
      --public   drop the PR links (the source repositories are private, so
                 they would be broken links on the public sentient-release)
      --summary  print only the first paragraph, the one-line summary that
                 the Platform Manager shows beside an update

  release-notes.py body <CHANGELOG.md> <version> --product gateway|mobile|spm
      The whole public release body: the notes (--public) and what to
      download, for a release made by hand on sentient-release (gateway,
      mobile app) or the Platform Manager's own repository. SENTIENT
      releases get theirs from publish-sentient-release.sh --changelog.

  release-notes.py update <CHANGELOG.md> <version> --product sentient|gateway|mobile|spm
                         [--tag <tag>] [--docker-image <image>] [--backup <dir>] [--dry-run]
      Replace the notes of a published release with this file's section and
      a downloads list made from the release's actual files: to give an older
      release proper notes, or fix them after publishing. The release is on
      QuakeString/sentient-release (tag <version>, gateway-<version>,
      SENTIENT-mobile-app-<version>) or, for spm, on SENTIENT_Platform_Manager
      (tag v<version>); --tag overrides. The old notes are saved in --backup
      (default ./release-notes-backup). Needs GITHUB_TOKEN or a github.com
      login saved in git.

  release-notes.py check <CHANGELOG.md> <version>
      Exit 1, saying why, when <version> has no section, no summary
      paragraph or no entries. Run it before publishing a release.

  release-notes.py draft <repo-dir> <from-ref> [<to-ref>] [--github owner/repo]
      Draft entries for the changes in <from-ref>..<to-ref> (default HEAD)
      from the first-parent history: merged PRs by their title (looked up
      with `gh` when --github is given and gh is signed in), other commits
      by their subject, sorted into Added / Changed / Fixed / Removed by
      their wording. Edit the draft into the Unreleased section by hand;
      it is a starting point, not the notes.

A version section looks like:

  ## [4.3.0.58] - 2026-11-05

  One or two sentences on what this release brings.   <- the summary

  Built from master `abcd1234`.

  ### Added
  - A user-visible change, in plain words ([#160](https://github.com/...)).

PR links go in a trailing parenthesis of an entry, so --public can drop them
without leaving broken sentences.
"""
import re
import subprocess
import sys

HEADING = re.compile(r"^## \[(?P<v>[^\]]+)\](?: [-\u2013\u2014] (?P<date>\S+))?\s*$")
PR_LINK = r"\[#\d+\]\([^)]*\)"
PR_PAREN = re.compile(r"\s*\((?:" + PR_LINK + r"|[\s,]|\band\b|\bto\b)+\)")


def section(text, version):
    lines = text.splitlines()
    start = None
    for i, line in enumerate(lines):
        m = HEADING.match(line)
        if m and start is None and m.group("v") == version:
            start = i + 1
        elif start is not None and (m or re.match(r"^\[[^\]]+\]:\s", line)):
            return "\n".join(lines[start:i]).strip("\n")
    if start is None:
        return None
    return "\n".join(lines[start:]).strip("\n")


def make_public(body):
    body = PR_PAREN.sub("", body)
    left = re.findall(PR_LINK, body)
    if left:
        sys.stderr.write(f"warning: PR links outside a trailing parenthesis, dropped: {', '.join(left)}\n")
        body = re.sub(r"\s*" + PR_LINK, "", body)
    return body


def unwrap(body):
    """Join hard-wrapped lines: GitHub shows every newline of a release body."""
    out = []
    for line in body.splitlines():
        start = re.match(r"^\s*([-*] |\d+\. |#|\||```|>)", line)
        prev = out[-1] if out else ""
        if line.strip() and not start and prev.strip() and not re.match(r"^\s*(#|\||```)", prev):
            out[-1] = prev.rstrip() + " " + line.strip()
        else:
            out.append(line)
    return "\n".join(out)


def summary(body):
    for para in re.split(r"\n\s*\n", body.strip()):
        para = para.strip()
        if para and not para.startswith(("#", "-", "*")):
            return " ".join(para.split())
    return ""


def cmd_extract(args):
    path, version = args[0], args[1]
    body = section(open(path, encoding="utf-8").read(), version)
    if body is None:
        sys.exit(f"{path}: no section for {version}")
    if "--public" in args:
        body = make_public(body)
    body = unwrap(body)
    print(summary(body) if "--summary" in args else body)


DOWNLOADS = {
    "gateway": """### Downloads
- Windows: run `SentientGatewaySetup-{v}.exe` over an earlier version to update; the configuration and the page password are kept.
- Linux: .deb (amd64, arm64, armhf), .rpm (x86_64, aarch64, armv7hl), Arch packages (x86_64, aarch64, armv7h) and the static binaries (.tar.gz); checksums in `SHA256SUMS`. A package installs `/usr/bin/sentient-gateway` with the systemd service `sentient-gateway` (settings `/etc/sentient-gateway`, data `/var/lib/sentient-gateway`, kept on removal); open http://127.0.0.1:8642/ on that machine.
- Docker: `quakestring/sentient-gateway-rs:{v}`""",
    "mobile": """### Downloads
- Android: `SENTIENT-mobile-app-{v}` APKs for arm64 and armeabi-v7a. Install the one for your phone; if Android refuses an update, uninstall the old app first.""",
    "spm": """### Downloads
- Windows: `SENTIENT-Setup-*.exe`. Linux x86_64: the `SENTIENT-Setup-*-linux-x86_64` program, or the Arch package.
- An installed Platform Manager offers this version itself on its update channel.""",
}


def cmd_body(args):
    if "--product" not in args:
        sys.exit("body needs --product " + "|".join(DOWNLOADS))
    product = args[args.index("--product") + 1]
    if product not in DOWNLOADS:
        sys.exit("unknown product " + product)
    path, version = args[0], args[1]
    body = section(open(path, encoding="utf-8").read(), version)
    if body is None:
        sys.exit(f"{path}: no section for {version}")
    print(unwrap(make_public(body)) + "\n\n" + DOWNLOADS[product].format(v=version))


def github_token():
    import os
    token = os.environ.get("GITHUB_TOKEN", "")
    if not token:
        try:
            out = subprocess.run(["git", "credential", "fill"], input="protocol=https\nhost=github.com\n\n",
                                 capture_output=True, text=True, check=True).stdout
            token = next((l[9:] for l in out.splitlines() if l.startswith("password=")), "")
        except (OSError, subprocess.CalledProcessError):
            pass
    if not token:
        sys.exit("no GitHub token (set GITHUB_TOKEN or save a github.com login in git)")
    return token


def github(method, url, token, data=None):
    import json
    import urllib.request
    req = urllib.request.Request(url, method=method, data=None if data is None else json.dumps(data).encode(),
                                 headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
                                          "Content-Type": "application/json"})
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def downloads_from(product, version, assets, docker_image):
    names = [a["name"] for a in assets]
    lines = ["### Downloads"]
    if product == "sentient":
        if "sentient-payload.tar" in names:
            lines.append("- Windows x64: `sentient-payload.tar`, with PostgreSQL 18.6, TimescaleDB and the VC++ runtime")
        for n, label in (("sentient-payload-linux-x86_64.tar", "Linux x86_64"), ("sentient-payload-linux-aarch64.tar", "Linux arm64")):
            if n in names:
                lines.append(f"- {label}: `{n}`")
        if docker_image:
            lines.append(f"- Docker: `{docker_image}`")
        lines.append("- `manifest.json` lists every file with its SHA-256. Install and update with the SENTIENT Platform Manager.")
        return "\n".join(lines)
    if product == "gateway":
        full = DOWNLOADS["gateway"].format(v=version).splitlines()
        keep = [l for l in full[1:] if not l.startswith("- Linux") or any(n.endswith(".deb") for n in names)]
        return "\n".join([full[0]] + keep)
    return DOWNLOADS[product].format(v=version)


def cmd_update(args):
    import json
    import os
    def opt(name, default=None):
        return args[args.index(name) + 1] if name in args else default
    path, version = args[0], args[1]
    product = opt("--product")
    if product not in ("sentient", "gateway", "mobile", "spm"):
        sys.exit("update needs --product sentient|gateway|mobile|spm")
    repo = "QuakeString/SENTIENT_Platform_Manager" if product == "spm" else "QuakeString/sentient-release"
    tag = opt("--tag") or {"sentient": version, "gateway": f"gateway-{version}",
                           "mobile": f"SENTIENT-mobile-app-{version}", "spm": f"v{version}"}[product]
    body = section(open(path, encoding="utf-8").read(), version)
    if body is None:
        sys.exit(f"{path}: no section for {version}")
    token = github_token()
    api = f"https://api.github.com/repos/{repo}"
    release = github("GET", f"{api}/releases/tags/{tag}", token)
    notes = unwrap(make_public(body)) + "\n\n" + downloads_from(product, version, release.get("assets", []), opt("--docker-image"))
    if "--dry-run" in args:
        print(notes)
        return
    backup = opt("--backup", "release-notes-backup")
    os.makedirs(backup, exist_ok=True)
    with open(os.path.join(backup, f"{repo.split('/')[1]}_{tag}.md"), "w", encoding="utf-8") as f:
        f.write(release.get("body") or "")
    github("PATCH", f"{api}/releases/{release['id']}", token, {"body": notes})
    print(f"{repo} {tag}: notes updated (old notes in {backup})")


def cmd_check(args):
    path, version = args[0], args[1]
    body = section(open(path, encoding="utf-8").read(), version)
    problems = []
    if body is None:
        problems.append(f"no '## [{version}] - <date>' section")
    else:
        if not summary(body):
            problems.append("no summary paragraph before the first ### heading")
        if not re.search(r"^### ", body, re.M) or not re.search(r"^- ", body, re.M):
            problems.append("no ### Added / Changed / Fixed / Removed entries")
    if problems:
        sys.exit(f"{path} {version}: " + "; ".join(problems))
    print(f"{path} {version}: ok")


KINDS = [
    ("Removed", re.compile(r"^(remove|drop|delete)|\bremove[ds]?\b", re.I)),
    ("Fixed", re.compile(r"^fix|\bfix(es|ed)?\b|no longer|\bagain\b|\bcrash|\berror\b|\bbug\b", re.I)),
    ("Added", re.compile(r"^(feat|add|new|support)|\badd(s|ed)?\b|\bnew\b", re.I)),
]


def kind_of(title):
    for kind, rx in KINDS:
        if rx.search(title):
            return kind
    return "Changed"


def clean(title):
    title = re.sub(r"^(feat|fix|chore|docs|build|ci|perf|refactor|test|style|i18n|ui)(\([^)]*\))?!?:\s*", "", title)  # conventional prefix
    title = re.sub(r"\s*\(#\d+\)$", "", title)
    return title[:1].upper() + title[1:]


def cmd_draft(args):
    gh = None
    if "--github" in args:
        i = args.index("--github")
        gh = args[i + 1]
        args = args[:i] + args[i + 2:]
    repo, frm = args[0], args[1]
    to = args[2] if len(args) > 2 else "HEAD"
    log = subprocess.run(["git", "-C", repo, "log", "--first-parent", "--format=%h%x09%s", f"{frm}..{to}"],
                         check=True, capture_output=True, text=True).stdout
    groups = {k: [] for k in ("Added", "Changed", "Fixed", "Removed")}
    for line in log.splitlines():
        sha, subject = line.split("\t", 1)
        if re.match(r"Merge (branch|remote-tracking branch) '?(origin/)?master", subject):
            continue
        pr = None
        m = re.match(r"Merge pull request #(\d+) ", subject) or re.search(r"\(#(\d+)\)$", subject)
        if m:
            pr = m.group(1)
        title = subject
        if pr and subject.startswith("Merge pull request") and gh:
            try:
                title = subprocess.run(["gh", "api", f"repos/{gh}/pulls/{pr}", "--jq", ".title"],
                                       check=True, capture_output=True, text=True).stdout.strip() or subject
            except (OSError, subprocess.CalledProcessError):
                pass
        title = clean(re.sub(r"^Merge [^:]*: ", "", title))
        ref = f" ([#{pr}](https://github.com/{gh}/pull/{pr}))" if pr and gh else (f" (#{pr})" if pr else f" ({sha})")
        groups[kind_of(subject if subject.startswith(("fix", "feat")) else title)].append(f"- {title}{ref}")
    for kind, items in groups.items():
        if items:
            print(f"### {kind}")
            print("\n".join(items))
            print()


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("extract", "body", "update", "check", "draft"):
        sys.exit(__doc__)
    {"extract": cmd_extract, "body": cmd_body, "update": cmd_update, "check": cmd_check, "draft": cmd_draft}[sys.argv[1]](sys.argv[2:])


if __name__ == "__main__":
    main()
