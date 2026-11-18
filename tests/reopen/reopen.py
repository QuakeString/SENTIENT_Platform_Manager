#!/usr/bin/env python3
# Copyright © 2016-2026 The SENTIENT Authors
#
# Reopen test: the window closed and opened again from the tray while the
# Platform Manager keeps running shows the page it was left on, with what
# it read, from its first frame: never the loading skeleton.
#
# The real app under its own Xvfb and D-Bus session (no keyring of the
# desktop's), in a throwaway profile. A debug build's SPM_DEBUG_FAKE_TRAY
# keeps it running with the window closed (Xvfb has no tray); the window is
# closed and opened again with the tray icon's own command (`toggle` on the
# single-instance socket, crate::background::listen), which runs the same
# bg_app::show the tray click does. The app's timing marks (SPM_DEBUG_TIMING)
# say what each window's first frame drew (`page-first-frame … drawn=`).
#
#   tests/reopen/reopen.py <debug binary> <out dir> local [--page statistics|logs]
#   tests/reopen/reopen.py <debug binary> <out dir> remote \
#       --host 127.0.0.1 --port 2242 --user ops --key <private key>
#
# local: this computer's Status (the installation record is copied into the
# profile and only read). remote: the server's Status; a bare server shows
# its "set up" state (drawn=state), one with the agent its data.
# --page: the first window goes to that page before it is closed, and the
# reopened one must show it with what it read: statistics (local or remote),
# logs (local: the lines shown, not "No log lines" before the first read).
# Asserts: the first window's first frame is the skeleton (a cold start),
# the reopened window's first frame is not, on the same page and instance,
# and that frame comes within --frame-ms of the reopen. Exit status 1 on a
# failure. Screenshots 250 ms after each window's first frame, and of the first
# window settled, are in the out dir.
import argparse, json, os, random, re, shutil, socket, subprocess, sys, time

ap = argparse.ArgumentParser()
ap.add_argument("bin"); ap.add_argument("out"); ap.add_argument("where", choices=["local", "remote"])
ap.add_argument("--host", default="127.0.0.1"); ap.add_argument("--port", type=int, default=2242)
ap.add_argument("--user", default="ops"); ap.add_argument("--key", default="")
ap.add_argument("--page", default="", help="go to this page (statistics, logs) in the first window before it is closed")
ap.add_argument("--settle", type=float, default=12., help="seconds the first window reads before it is closed")
ap.add_argument("--frame-ms", type=float, default=50., help="the reopened window's first frame within this of the window opening")
a = ap.parse_args()

out = os.path.abspath(os.path.join(a.out, a.where + (f"-{a.page}" if a.page else "")))
shutil.rmtree(out, ignore_errors=True)
xdg, data = os.path.join(out, "xdg"), os.path.join(out, "data")
cfg = os.path.join(xdg, "sentient-platform-manager")
os.makedirs(cfg); os.makedirs(data)
log_path = os.path.join(out, "app.log")

session = {"page": "status"}
if a.where == "remote":
    scan = subprocess.run(["ssh-keyscan", "-t", "ed25519", "-p", str(a.port), a.host], capture_output=True, text=True, timeout=20).stdout
    if not scan.strip():
        sys.exit("reopen: the server does not answer")
    with open(os.path.join(cfg, "known_hosts"), "w") as f:
        f.write(scan)
    fp = subprocess.run(["ssh-keygen", "-lf", "-"], input=scan, capture_output=True, text=True).stdout.split()[1]
    inst = {"id": "reopen", "name": "Reopen test", "color": "#6b4fa0", "os": "linux", "host": a.host, "port": a.port,
            "user": a.user, "auth": "key", "key_file": os.path.abspath(a.key), "key_has_passphrase": False, "host_key": fp}
    with open(os.path.join(cfg, "instances.json"), "w") as f:
        json.dump([inst], f)
    session["instance"] = "reopen"
with open(os.path.join(cfg, "session.json"), "w") as f:
    json.dump(session, f)
record = os.path.join(out, "installation.json")
if a.where == "local":
    shutil.copy("/var/lib/sentient-platform-manager/installation.json", record)
else:
    record = "/nonexistent"

W, H = 1400, 900
for _ in range(50):
    disp = f":{2000 + random.randrange(700)}"
    xv = subprocess.Popen(["Xvfb", disp, "-screen", "0", f"{W}x{H}x24", "-nolisten", "tcp"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    if xv.poll() is None and os.path.exists(f"/tmp/.X11-unix/X{disp[1:]}"):
        break
env = dict(os.environ, DISPLAY=disp, WAYLAND_DISPLAY="", XDG_CONFIG_HOME=xdg, XDG_DATA_HOME=data,
           SPM_WINDOW_SIZE=f"{W}x{H}", SPM_INSTALLATION_RECORD=record, SPM_THEME="light",
           SPM_DEBUG_TIMING="1", SPM_DEBUG_FAKE_TRAY="1")
env.pop("DBUS_SESSION_BUS_ADDRESS", None)
if a.page:
    env["SPM_DEBUG_GO_PAGES"] = f"{a.page}:2"
log = open(log_path, "w")
# Its own session bus: the desktop's keyring and tray are never reached.
app = subprocess.Popen(["dbus-run-session", "--", a.bin, "--manager"], env=env, stdout=subprocess.DEVNULL, stderr=log)

MARK = re.compile(r"^TIMING (\S+) (.*?) ?ms=([\d.]+) epoch_ms=(\d+)$")
failures = []


def marks():
    got = []
    with open(log_path, errors="replace") as f:
        for l in f:
            m = MARK.match(l.strip())
            if m:
                got.append((m.group(1), dict(re.findall(r"(\w+)=(\S*)", m.group(2))), float(m.group(3))))
    return got


def wait_mark(name, n, timeout=30.):
    """The n-th (from 1) mark `name`, or None."""
    end = time.time() + timeout
    while time.time() < end:
        ms = [m for m in marks() if m[0] == name]
        if len(ms) >= n:
            return ms[n - 1]
        if app.poll() is not None:
            return None
        time.sleep(0.005)
    return None


def shot(name):
    subprocess.run(["import", "-display", disp, "-window", "root", os.path.join(out, name + ".png")], capture_output=True, timeout=20)


def send(cmd):
    port, token = open(os.path.join(cfg, "running.lock")).read().split()[:2]
    with socket.create_connection(("127.0.0.1", int(port)), timeout=5) as s:
        s.sendall(f"{token} {cmd}\n".encode())
        return s.recv(16).decode().strip()


def windows():
    r = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "."], env=dict(os.environ, DISPLAY=disp), capture_output=True, text=True)
    return [w for w in r.stdout.split() if w]


def check(ok, what):
    print(("ok   " if ok else "FAIL ") + what)
    if not ok:
        failures.append(what)


try:
    first = wait_mark("page-first-frame", 1, 60.)
    check(first is not None, "the first window draws")
    if first:
        # Shortly after (the frame presented; a cold remote still reads).
        time.sleep(0.25)
        shot("1-cold-250ms")
        check(first[1].get("drawn") == "skeleton", f"cold start: the first frame is the loading skeleton (drawn={first[1].get('drawn')})")
    time.sleep(a.settle)
    shot("2-cold-settled")
    # The tray icon's click on an open window: it closes (the app runs on).
    check(send("toggle") == "ok", "tray click: the window closes")
    time.sleep(1.5)
    check(app.poll() is None, "the app keeps running with the window closed")
    check(not windows(), "no window is open")
    # The tray icon's click again: the window opens.
    t0 = time.time()
    check(send("toggle") == "ok", "tray click: the window opens again")
    again = wait_mark("page-first-frame", 2, 30.)
    click_ms = (time.time() - t0) * 1000
    check(again is not None, "the reopened window draws")
    if again:
        time.sleep(0.25)
        shot("3-reopen-250ms")
        time.sleep(0.75)
        shot("4-reopen-1s")
        show = [m for m in marks() if m[0] == "show-window"]
        opened = [m for m in marks() if m[0] == "window-opened"]
        drawn = again[1].get("drawn")
        print(f"     reopen: show-window to first frame {again[2] - show[-1][2]:.1f} ms, window-opened to first frame {again[2] - opened[-1][2]:.1f} ms, tray click to the mark seen {click_ms:.0f} ms")
        check(drawn in ("data", "state"), f"reopen: the first frame is the page with what it read, not the skeleton (drawn={drawn})")
        check(again[1].get("page") == (a.page or first[1].get("page")) and again[1].get("instance") == first[1].get("instance"),
              f"reopen: the same page and instance ({again[1].get('page')}@{again[1].get('instance')})")
        check(again[2] - opened[-1][2] <= a.frame_ms, f"reopen: the page's first frame within {a.frame_ms:.0f} ms of the window opening ({again[2] - opened[-1][2]:.1f} ms)")
finally:
    try:
        send("action exit")
    except OSError:
        pass
    try:
        app.wait(10)
    except subprocess.TimeoutExpired:
        app.kill()
    xv.kill()
    log.close()

print(f"reopen {a.where}: {'ok' if not failures else 'FAIL'} (log and screenshots in {out})")
sys.exit(1 if failures else 0)
