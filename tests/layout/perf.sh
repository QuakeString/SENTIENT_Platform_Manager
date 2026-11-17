#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Performance probe: one run of the Platform Manager under Xvfb on one
# page, with its SPM_DEBUG_TIMING marks (crate::timing), its processor
# time and memory from /proc, and (EXECS=1) every program it started.
#
#   tests/layout/perf.sh <app binary> <out dir> <page> <seconds> [instance id]
#
# PROFILE: an XDG_CONFIG_HOME to copy (instances); RECORD: an installation
# record (default: none). DOCKER_SHIM=1 puts a `docker` first on PATH that
# only logs its arguments to <out>/docker.calls and answers nothing, so
# the run never touches the containers on this computer. AFTER: pages to
# visit in turn (xdotool clicks are not needed: SPM_DEBUG_GO_PAGES).
#
# Prints one line: key=value pairs (first_frame_ms, cpu_ms, rss_kb, …).
set -u
BIN=${1:?binary}; OUT=${2:?out dir}; PAGE=${3:?page}; SECS=${4:?seconds}; INST=${5:-}
mkdir -p "$OUT"
x=$(mktemp -d "$OUT/xdg.XXXX")
[ -n "${PROFILE:-}" ] && cp -r "$PROFILE"/. "$x"/ && rm -f "$x"/sentient-platform-manager/running.lock
path=$PATH
if [ -n "${DOCKER_SHIM:-}" ]; then
  mkdir -p "$OUT/shim"
  cat >"$OUT/shim/docker" <<EOF
#!/bin/sh
echo "\$*" >> "$OUT/docker.calls"
exit 0
EOF
  chmod +x "$OUT/shim/docker"; : >"$OUT/docker.calls"; path="$OUT/shim:$PATH"
fi
for n in $(seq 1 50); do
  d=":$((200 + RANDOM % 700))"
  Xvfb "$d" -screen 0 1200x800x24 -nolisten tcp >/dev/null 2>&1 & xp=$!
  sleep 1
  if kill -0 $xp 2>/dev/null && [ -e "/tmp/.X11-unix/X${d#:}" ]; then break; fi
done
inst=(); [ -n "$INST" ] && inst=(--instance "$INST")
wrap=(); [ -n "${EXECS:-}" ] && wrap=(strace -f -qq -e trace=execve -e signal=none -o "$OUT/execs.trace")
t0=$(date +%s%3N)
env PATH="$path" GPUI_X11_SCALE_FACTOR=1 DISPLAY=$d WAYLAND_DISPLAY= XDG_CONFIG_HOME=$x SPM_WINDOW_SIZE=1100x720 \
  SPM_INSTALLATION_RECORD=${RECORD:-/nonexistent} SPM_REDUCED_MOTION=1 SPM_DEBUG_TIMING=1 ${EXTRA_ENV:-} \
  "${wrap[@]}" "$BIN" --manager --page "$PAGE" "${inst[@]}" 2>"$OUT/app.log" >/dev/null & ap=$!
sleep 1
# The app's own process (strace's child when wrapped).
pid=$ap; [ -n "${EXECS:-}" ] && pid=$(pgrep -P $ap | head -1)
# Processor time after the start settles (from 5 s on), so the idle part is
# its own number.
sleep 4
read -r u1 s1 cu1 cs1 < <(awk '{print $14, $15, $16, $17}' /proc/$pid/stat)
t1=$(date +%s%3N)
sleep $((SECS > 5 ? SECS - 5 : 1))
read -r u2 s2 cu2 cs2 < <(awk '{print $14, $15, $16, $17}' /proc/$pid/stat)
t2=$(date +%s%3N)
rss=$(awk '/VmRSS/{print $2}' /proc/$pid/status); hwm=$(awk '/VmHWM/{print $2}' /proc/$pid/status)
thr=$(awk '/Threads/{print $2}' /proc/$pid/status)
kill $ap 2>/dev/null; sleep 1; kill -9 $ap $pid 2>/dev/null; kill $xp 2>/dev/null; rm -rf "$x"
tick=$(getconf CLK_TCK)
ff=$(awk -v t0="$t0" '/^TIMING first-frame/{for(i=1;i<=NF;i++) if($i ~ /^epoch_ms=/){split($i,a,"="); print a[2]-t0; exit}}' "$OUT/app.log")
fr=$(grep -m1 '^TIMING first-render' "$OUT/app.log" | sed 's/.*render_ms=\([0-9.]*\).*/\1/')
renders=$(grep '^TIMING renders' "$OUT/app.log" | sed 's/.*per_s=\([0-9.]*\).*/\1/' | tr '\n' ',' )
idle_cpu_ms=$(( ( (u2+s2+cu2+cs2) - (u1+s1+cu1+cs1) ) * 1000 / tick ))
idle_s=$(( (t2 - t1) / 1000 ))
docker_calls=$( [ -n "${DOCKER_SHIM:-}" ] && wc -l <"$OUT/docker.calls" || echo -)
execs=$( [ -n "${EXECS:-}" ] && grep -c 'execve(' "$OUT/execs.trace" || echo -)
echo "page=$PAGE${INST:+@$INST} launch_to_first_frame_ms=${ff:-?} first_render_ms=${fr:-?} idle_s=$idle_s idle_cpu_ms=$idle_cpu_ms cpu_pct=$(python3 -c "print(round($idle_cpu_ms/(max($idle_s,1)*10),2))") rss_kb=$rss hwm_kb=$hwm threads=$thr renders_per_s=${renders:-?} docker_calls=$docker_calls execs=$execs"
