#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
#
# Files click test: the real app under its own Xvfb, driven with xdotool.
# On each pane (this computer on the left, a server on the right) it
# double-clicks a folder, clicks a path-bar segment, presses Enter on a
# selected folder, clicks the place buttons, opens a folder it may not read
# (the path goes back, the reason shows), and clicks twice in a row while a
# listing is held back (the newest click wins). For each it asserts the path
# bar shows the new path within --bar-ms of the click and the listing within
# --open-ms, from the app's own timing marks (SPM_DEBUG_TIMING) and where it
# drew each control (SPM_DEBUG_LAYOUT).
#
#   tests/files/clicks.py <debug binary> <out dir> --profile <XDG_CONFIG_HOME>
#       --instance <id> --remote-base <folder on the server>
#       --local-base <folder here> [--sides 0,1] [--theme light|dark]
#
# Each base folder holds `big/` (folders dir-01 … dir-40 and 60 files),
# `fresh/` (opened only by the rapid case) and `locked/` (mode 000);
# tests/files/fixture.sh makes them. --no-ahead: no reading ahead, so each
# first visit shows the skeleton rows (asserted). Exit status 1 on a failure.
import argparse, json, os, random, re, shutil, subprocess, sys, time

ap = argparse.ArgumentParser()
ap.add_argument("bin"); ap.add_argument("out")
ap.add_argument("--profile", required=True); ap.add_argument("--instance", required=True)
ap.add_argument("--remote-base", required=True); ap.add_argument("--local-base", required=True)
ap.add_argument("--remote-home", default="")
ap.add_argument("--sides", default="0,1"); ap.add_argument("--theme", default="light")
ap.add_argument("--bar-ms", type=float, default=50.); ap.add_argument("--open-ms", type=float, default=300.)
ap.add_argument("--hold-ms", type=int, default=700, help="how long the rapid-click case holds a listing back")
ap.add_argument("--no-ahead", action="store_true", help="no reading ahead: every folder opens uncached (the skeleton)")
a = ap.parse_args()

os.makedirs(a.out, exist_ok=True)
log_path = os.path.join(a.out, f"app-{a.theme}.log")
xdg = os.path.join(a.out, f"xdg-{a.theme}")
data = os.path.join(a.out, f"data-{a.theme}")
for d in (xdg, data):
    shutil.rmtree(d, ignore_errors=True)
shutil.copytree(a.profile, xdg)
os.makedirs(data)
try:
    os.remove(os.path.join(xdg, "sentient-platform-manager", "running.lock"))
except FileNotFoundError:
    pass
hold = os.path.join(a.out, "hold-ms")
try:
    os.remove(hold)
except FileNotFoundError:
    pass

W, H = 1400, 900
for _ in range(50):
    disp = f":{2000 + random.randrange(700)}"
    xv = subprocess.Popen(["Xvfb", disp, "-screen", "0", f"{W}x{H}x24", "-nolisten", "tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    if xv.poll() is None and os.path.exists(f"/tmp/.X11-unix/X{disp[1:]}"):
        break
env = dict(os.environ, DISPLAY=disp, WAYLAND_DISPLAY="", XDG_CONFIG_HOME=xdg, XDG_DATA_HOME=data,
           SPM_THEME=a.theme, SPM_WINDOW_SIZE=f"{W}x{H}", SPM_INSTALLATION_RECORD="/nonexistent",
           SPM_DEBUG_TIMING="1", SPM_DEBUG_LAYOUT="1", SPM_NO_TRAY="1",
           SPM_DEBUG_FILES_START=f"{a.local_base}|{a.remote_base}", SPM_DEBUG_FILES_HOLD=hold)
if a.no_ahead:
    env["SPM_DEBUG_FILES_NO_AHEAD"] = "1"
log = open(log_path, "w")
app = subprocess.Popen([a.bin, "--manager", "--instance", a.instance, "--page", "files"], env=env, stdout=subprocess.DEVNULL, stderr=log)

results, failures = [], []
pos = 0
lines = []
probes = {}
MARK = re.compile(r"^TIMING (\S+) (.*) epoch_ms=(\d+)$")


def pump():
    """New lines of the app's log: probes kept, marks returned."""
    global pos
    with open(log_path, errors="replace") as f:
        f.seek(pos)
        chunk = f.read()
    cut = chunk.rfind("\n") + 1
    pos += len(chunk[:cut].encode())
    for l in chunk[:cut].splitlines():
        if l.startswith("LAYOUT files-"):
            p = l.split()
            probes[" ".join(p[1:-4])] = tuple(int(v) for v in p[-4:])
        m = MARK.match(l)
        if m:
            kv = dict(re.findall(r"(\w+)=(\S*)", m.group(2)))
            lines.append((m.group(1), kv, int(m.group(3))))


def wait(pred, timeout=20., since=0):
    end = time.time() + timeout
    while time.time() < end:
        pump()
        for i in range(since, len(lines)):
            if pred(*lines[i]):
                return i, lines[i]
        time.sleep(0.01)
    return None, None


def bar(side, path=None, rows=None, skeleton=None, error=None):
    def p(what, kv, _):
        return (what == "files-bar" and kv.get("side") == str(side) and (path is None or kv.get("path") == path)
                and (rows is None or (int(kv.get("rows", 0)) > 0) == rows) and (skeleton is None or kv.get("skeleton") == skeleton)
                and (error is None or (kv.get("error", "") != "") == error))
    return p


def xdo(*args):
    subprocess.run(["xdotool", *args], env=env, check=True)


def centre(name):
    end = time.time() + 5
    while name not in probes and time.time() < end:
        time.sleep(0.02)
        pump()
    time.sleep(0.05)
    pump()
    l, r, t, b = probes[name]
    return (l + r) // 2, (t + b) // 2


def shot(name):
    subprocess.run(["import", "-window", "root", os.path.join(a.out, f"{name}-{a.theme}.png")], env=env)


def act(side, label, do, want, check_skeleton=False, timeout=20., from_nav=False):
    """Do a click; the bar must show `want` within --bar-ms and the rows
    within --open-ms. `from_nav`: timed from when the page took the input
    (a key: X's key delivery to the window is not the page's)."""
    since = len(lines)
    do()
    t = time.time() * 1000
    i, hit = wait(bar(side, want), timeout, since)
    if hit is None:
        failures.append(f"side {side} {label}: the bar never showed {want}")
        return None
    if from_nav:
        _, nav = wait(lambda w, kv, _: w == "files-nav" and kv.get("side") == str(side) and kv.get("path") == want, 1., since)
        if nav:
            t = nav[2]
    bar_ms = hit[2] - t
    j, full = wait(bar(side, want, rows=True), timeout, since)
    open_ms = full[2] - t if full else None
    listed = [kv for w, kv, _ in lines[since:] if w == "files-listed" and kv.get("side") == str(side)]
    r = {"side": side, "case": label, "bar_ms": round(max(bar_ms, 0)), "open_ms": None if open_ms is None else round(max(open_ms, 0)),
         "rows_at_once": hit[1].get("rows", "0") != "0", "listings": listed}
    results.append(r)
    if bar_ms > a.bar_ms:
        failures.append(f"side {side} {label}: the bar showed the new path after {bar_ms:.0f} ms (> {a.bar_ms:.0f})")
    if open_ms is None:
        failures.append(f"side {side} {label}: the rows of {want} never showed")
    elif open_ms > a.open_ms:
        failures.append(f"side {side} {label}: the rows showed after {open_ms:.0f} ms (> {a.open_ms:.0f})")
    if check_skeleton and not r["rows_at_once"]:
        if not any(w == "files-bar" and kv.get("side") == str(side) and kv.get("path") == want and kv.get("skeleton") == "true" for w, kv, _ in lines[since:]):
            failures.append(f"side {side} {label}: no skeleton while {want} loaded")
    return r


def dbl(name):
    return lambda: xdo("mousemove", *map(str, centre(name)), "click", "--repeat", "2", "--delay", "60", "1")


def click(name):
    return lambda: xdo("mousemove", *map(str, centre(name)), "click", "1")


def join(base, *p):
    return "/".join([base.rstrip("/"), *p])


def crumb(side, path):
    return f"files-crumb {side} {len(path.strip('/').split('/')) - 1}"


try:
    for side in map(int, a.sides.split(",")):
        base = a.local_base if side == 0 else a.remote_base
        home = os.environ["HOME"] if side == 0 else a.remote_home
        _, ok = wait(bar(side, base, rows=True), 60.)
        if ok is None:
            failures.append(f"side {side}: {base} never showed")
            continue
        time.sleep(1.5)  # the prefetch after the first listing settles
        big = join(base, "big")
        act(side, "double-click a folder", dbl(f"files-row {side} big"), big, check_skeleton=True)
        act(side, "double-click a subfolder", dbl(f"files-row {side} dir-05"), join(big, "dir-05"), check_skeleton=True)
        act(side, "path-bar segment", click(crumb(side, big)), big)
        def enter():
            click(f"files-row {side} dir-07")()
            time.sleep(0.6)
            xdo("key", "Return")
        act(side, "Enter on a selected folder", enter, join(big, "dir-07"), check_skeleton=True, from_nav=True)
        act(side, "path-bar segment, base", click(crumb(side, base)), base)
        # Failure: a folder it may not read. The bar shows it at once, then
        # goes back to the base with the reason under it.
        since = len(lines)
        dbl(f"files-row {side} locked")()
        t = time.time() * 1000
        _, first = wait(bar(side, join(base, "locked")), 10., since)
        _, back = wait(bar(side, base, error=True), 10., since)
        r = {"side": side, "case": "unreadable folder: back with the reason", "bar_ms": first and round(first[2] - t), "back_ms": back and round(back[2] - t)}
        results.append(r)
        # The folder clicked shows in the bar at once, unless the failure
        # came back before the next frame (then the bar never left).
        if (not first or first[2] - t > a.bar_ms) and not (back and back[2] - t <= a.bar_ms):
            failures.append(f"side {side} unreadable: the bar did not show the folder clicked within {a.bar_ms:.0f} ms")
        if not back:
            failures.append(f"side {side} unreadable: the path did not go back to {base} with the reason")
        else:
            shot(f"side{side}-error")
        # Rapid: the next listing held back (SPM_DEBUG_FILES_HOLD), a second
        # click before it comes; the second wins and the first never shows.
        up = base.rstrip("/").rsplit("/", 1)[0] or "/"
        time.sleep(0.3)  # the rows moved under the reason: their new places
        open(hold, "w").write(str(a.hold_ms))
        since = len(lines)
        dbl(f"files-row {side} fresh")()
        time.sleep(0.25)
        shot(f"side{side}-skeleton")
        click(crumb(side, up))()
        t = time.time() * 1000
        _, won = wait(bar(side, up, rows=True), 20., since)
        time.sleep(a.hold_ms / 1000. + 0.5)
        pump()
        after = [kv.get("path") for w, kv, _ in lines[since:] if w == "files-bar" and kv.get("side") == str(side)]
        k = after.index(up) if up in after else len(after)
        stale = [p for p in after[k:] if p != up]
        results.append({"side": side, "case": "rapid second click wins", "won_ms": won and round(won[2] - t), "bar_after": after})
        if not won:
            failures.append(f"side {side} rapid: {up} never showed")
        if stale:
            failures.append(f"side {side} rapid: a stale listing showed after the second click: {stale}")
        if join(base, "fresh") not in after:
            failures.append(f"side {side} rapid: the first click never showed in the bar")
        elif a.no_ahead and not any(w == "files-bar" and kv.get("path") == join(base, "fresh") and kv.get("skeleton") == "true" for w, kv, _ in lines[since:]):
            failures.append(f"side {side} rapid: no skeleton rows while the held folder loaded")
        # The place buttons there are, last (they leave the base).
        pump()
        chips = sorted(k.split(" ", 2)[2] for k in probes if k.startswith(f"files-chip {side} "))
        targets = {"Home": home, "SENTIENT_folder": "/opt/sentient", "Backups": "/var/backups/sentient"}
        done = [c for c in chips if targets.get(c)]
        for c in done:
            act(side, f"place button {c}", click(f"files-chip {side} {c}"), targets[c])
        if side == 1 and len(done) < 3:
            failures.append(f"side 1: place buttons {done}, not Home, SENTIENT folder and Backups")
finally:
    shot("final")
    app.terminate()
    try:
        app.wait(5)
    except subprocess.TimeoutExpired:
        app.kill()
    xv.terminate()
    log.close()

json.dump({"results": results, "failures": failures}, open(os.path.join(a.out, f"results-{a.theme}.json"), "w"), indent=1)
for r in results:
    print(json.dumps({k: v for k, v in r.items() if k != "listings"}), *(f"\n    listed {l}" for l in r.get("listings", [])))
for f in failures:
    print("FAIL", f)
print("files clicks:", "FAIL" if failures else "ok", f"({len(results)} cases, {len(failures)} failures)")
sys.exit(1 if failures else 0)
