#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Performance gate (docs/analysis/SPM_PERFORMANCE_AUDIT.md): checks that
# the audit's fixes hold, on a debug build under Xvfb.
#
#   tests/layout/perf_gate.sh <app binary> <out dir>
#
# 1. hidden pages do not poll: Status opened, then About; from 8 s to 38 s
#    the docker calls are only the tray's (one each 15 s), not the Status
#    and gateway polls' (before: 14 in 30 s). A `docker` that only logs its
#    arguments is first on PATH: no container on this computer is touched.
# 2. the theme at start waits for the desktop 300 ms at most (before: a
#    desktop portal that never answers held every start 2 s; the limit
#    here leaves room for a loaded machine).
# 3. (reported, not a failure: it follows the machine's load) an idle
#    window's renders per second. The meters glide only for a change of a
#    pixel or more (unit test manager::perf_tests); before, 400 ms of
#    frames every 4 s.
#
# Exit status 1 on any failure.
set -u
BIN=${1:?binary}; OUT=${2:?out dir}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
REC="$OUT/record-docker.json"
# A Docker install whose stack is not there (the shim answers nothing).
cat >"$REC" <<'EOF'
{"sentient_version":"4.4.0.2","manager_version":"0.8.0","engine":"docker","install_dir":"/nonexistent/spm-perf","pg_dir":"/nonexistent/spm-perf/pg","state_dir":"/nonexistent/spm-perf/state","stack":{"http_port":1},"installed_at":"2026-11-09T10:00:00Z"}
EOF
fail=0

# 1. Hidden pages.
calls_between() { # <calls file> <log> <from s> <to s>: calls from..to after the first frame
  python3 - "$@" <<'PY'
import sys
calls, log, a, b = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4])
t0 = None
for l in open(log):
    if l.startswith("TIMING first-frame"):
        t0 = int(l.split("epoch_ms=")[1]); break
n = 0
for l in open(calls):
    t, _, args = l.partition(" ")
    if t0 is not None and a * 1000 <= int(t) - t0 < b * 1000:
        n += 1
print(n)
PY
}
mkdir -p "$OUT/shim"
cat >"$OUT/shim/docker" <<EOF
#!/bin/sh
echo "\$(date +%s%3N) \$*" >> "$OUT/docker.calls"
exit 0
EOF
chmod +x "$OUT/shim/docker"; : >"$OUT/docker.calls"
EXTRA_ENV="PATH=$OUT/shim:$PATH SPM_THEME=light SPM_REDUCE_MOTION=1 SPM_DEBUG_GO_PAGES=about:6" RECORD=$REC \
  "$HERE/perf.sh" "$BIN" "$OUT/hidden" status 40 >"$OUT/hidden.result"
n=$(calls_between "$OUT/docker.calls" "$OUT/hidden/app.log" 8 38)
if [ "$n" -le 3 ]; then echo "ok   hidden pages: $n docker calls in 30 s on About after Status (the tray's)"; else echo "FAIL hidden pages: $n docker calls in 30 s on About after Status (at most 3: the tray's)"; sort "$OUT/docker.calls" | cut -d' ' -f2- | sort | uniq -c; fail=1; fi

# 2. The theme at start ("follow system": no SPM_THEME).
EXTRA_ENV="SPM_REDUCE_MOTION=1" "$HERE/perf.sh" "$BIN" "$OUT/theme" about 6 >"$OUT/theme.result"
ms=$(python3 - "$OUT/theme/app.log" <<'PY'
import re, sys
m = {}
for l in open(sys.argv[1]):
    g = re.match(r"TIMING (\S+) ms=([0-9.]+)", l)
    if g:
        m[g.group(1)] = float(g.group(2))
print(round(m["theme-applied"] - m["theme-init"]) if "theme-applied" in m and "theme-init" in m else 99999)
PY
)
if [ "$ms" -le 1200 ]; then echo "ok   theme at start: $ms ms"; else echo "FAIL theme at start: $ms ms (at most 1200; 300 ms on a quiet machine)"; fail=1; fi

# 3. An idle window (motion on): renders per second in each 10 s after the
# first (the start's own animations).
EXTRA_ENV="SPM_THEME=light" "$HERE/perf.sh" "$BIN" "$OUT/idle" about 34 >"$OUT/idle.result"
r=$(grep '^TIMING renders' "$OUT/idle/app.log" | tail -n +2 | sed 's/.*per_s=\([0-9.]*\).*/\1/' | python3 -c "import sys; v=[float(x) for x in sys.stdin]; print(max(v) if v else 99)")
echo "info idle window: $r renders/s at most in a 10 s window (before: 2.4 to 3.9)"
exit $fail
