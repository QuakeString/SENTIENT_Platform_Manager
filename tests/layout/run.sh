#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Layout test: every page at the minimum window width and at 900 and 1200
# px, scale 1 and 1.5, light and dark, this computer and (with INSTANCE set
# to an instance id in the profile) a server; check.py fails a render where
# anything reaches into a card's right padding or the page's right margin.
#
#   tests/layout/run.sh <app binary> <out dir> [XDG_CONFIG_HOME with instances]
#
# Needs Xvfb and ImageMagick's import. Exit status 1 on any finding.
set -u
BIN=${1:?binary}; OUT=${2:?out dir}; PROFILE=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"
PAGES_LOCAL="status statistics control logs update gateway backup migration repair settings instances about"
PAGES_REMOTE="status statistics control logs update gateway backup migration repair settings"
WIDTHS=${WIDTHS:-"560 900 1200"}; SCALES=${SCALES:-"1 1.5"}; THEMES=${THEMES:-"light dark"}
JOBS=${JOBS:-4}
one() { # where page width scale theme
  local where=$1 page=$2 w=$3 sc=$4 th=$5 h=760
  local name="$where-$page-$w-x$sc-$th" x="$OUT/xdg-$where-$page-$w-$sc-$th"
  rm -rf "$x"; mkdir -p "$x"
  [ -n "$PROFILE" ] && cp -r "$PROFILE"/. "$x"/ && rm -f "$x"/sentient-platform-manager/running.lock
  local pw ph; pw=$(python3 -c "print(int($w*$sc))"); ph=$(python3 -c "print(int($h*$sc))")
  # A display of its own: Xvfb refuses one in use (its lock file), so try
  # numbers until one starts (parallel runs must never share a screen).
  local d xp n
  for n in $(seq 1 50); do
    d=":$((200 + RANDOM % 700))"
    Xvfb "$d" -screen 0 "${pw}x${ph}x24" -nolisten tcp >/dev/null 2>&1 & xp=$!
    sleep 1
    if kill -0 $xp 2>/dev/null && [ -e "/tmp/.X11-unix/X${d#:}" ]; then break; fi
  done
  local inst=(); [ "$where" = remote ] && inst=(--instance "${INSTANCE:?}")
  GPUI_X11_SCALE_FACTOR=$sc DISPLAY=$d WAYLAND_DISPLAY= XDG_CONFIG_HOME=$x SPM_THEME=$th SPM_WINDOW_SIZE=${w}x${h} \
    SPM_INSTALLATION_RECORD=${SPM_INSTALLATION_RECORD:-/nonexistent} SPM_DEBUG_LAYOUT=1 SPM_REDUCED_MOTION=1 \
    "$BIN" --manager --page "$page" "${inst[@]}" 2>"$OUT/$name.log" >/dev/null & local ap=$!
  sleep "${WAIT:-18}"
  DISPLAY=$d import -window root "$OUT/$name.png" 2>/dev/null
  kill $ap 2>/dev/null; sleep 1; kill -9 $ap 2>/dev/null; kill $xp 2>/dev/null
  rm -rf "$x"
  python3 "$HERE/check.py" "$OUT/$name.png" "$OUT/$name.log" "$sc" > "$OUT/$name.result" 2>&1 && echo "ok   $name" || { echo "FAIL $name"; cat "$OUT/$name.result"; }
}
export -f one; export OUT HERE BIN PROFILE INSTANCE WAIT SPM_INSTALLATION_RECORD 2>/dev/null
list=()
for p in $PAGES_LOCAL; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("local $p $w $s $t"); done; done; done; done
if [ -n "${INSTANCE:-}" ]; then
  for p in $PAGES_REMOTE; do for w in $WIDTHS; do for s in $SCALES; do for t in $THEMES; do list+=("remote $p $w $s $t"); done; done; done; done
fi
printf '%s\n' "${list[@]}" | xargs -P "$JOBS" -L 1 bash -c 'one "$@"' _ | tee "$OUT/summary.txt"
fails=$(grep -c '^FAIL' "$OUT/summary.txt"); total=$(grep -c -E '^(ok|FAIL)' "$OUT/summary.txt")
echo "layout test: $((total - fails))/$total passed"
[ "$fails" -eq 0 ]
