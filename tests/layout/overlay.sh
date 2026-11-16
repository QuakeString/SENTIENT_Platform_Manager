#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# Narrow-window menu overlay: at 560 and 900 px, scale 1/1.5/2, light and
# dark, this computer (and INSTANCE's Status when set), opens the menu with
# the logo's unfold button and checks that the page's boxes did not move
# (overlay_compare.py), the closed render passes check.py, icons.py passes
# open and closed, and Escape closes it.
#
#   tests/layout/overlay.sh <app binary> <out dir> [XDG_CONFIG_HOME with instances]
set -u
BIN=${1:?binary}; OUT=${2:?out dir}; PROFILE=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"; JOBS=${JOBS:-2}
one() { # name page theme width scale [instance]
  local n=$1 p=$2 th=$3 w=$4 sc=$5 inst=${6:-} h=720 x="$OUT/xdg-$1" D XP AP
  rm -rf "$x"; mkdir -p "$x"; [ -n "$PROFILE" ] && cp -r "$PROFILE"/. "$x"/ && rm -f "$x"/sentient-platform-manager/running.lock
  local pw ph s; pw=$(python3 -c "print(int($w*$sc))"); ph=$(python3 -c "print(int($h*$sc))"); s() { python3 -c "print(int($1*$sc))"; }
  for _ in $(seq 1 50); do D=:$((100 + RANDOM % 800)); Xvfb $D -screen 0 ${pw}x${ph}x24 -nolisten tcp >/dev/null 2>&1 & XP=$!; sleep 1; kill -0 $XP 2>/dev/null && [ -e /tmp/.X11-unix/X${D#:} ] && break; done
  env SPM_DEBUG_LAYOUT=1 SPM_REDUCED_MOTION=1 GPUI_X11_SCALE_FACTOR=$sc DISPLAY=$D WAYLAND_DISPLAY= XDG_CONFIG_HOME="$x" SPM_THEME=$th SPM_WINDOW_SIZE=${w}x$h SPM_INSTALLATION_RECORD=/nonexistent "$BIN" --manager --page $p ${inst:+--instance $inst} > "$OUT/$n.log" 2>&1 & AP=$!
  sleep ${WAIT:-20}
  DISPLAY=$D xdotool mousemove 5 $(s 400); sleep 1
  DISPLAY=$D import -window root "$OUT/$n-closed.png"; stat -c %s "$OUT/$n.log" > "$OUT/$n.split"
  DISPLAY=$D xdotool mousemove $(s 32) $(s 30); sleep 0.6; DISPLAY=$D xdotool click 1; sleep 1.2
  DISPLAY=$D xdotool mousemove $(python3 -c "print(int($w*0.75*$sc))") $(s 300); sleep 0.8
  DISPLAY=$D import -window root "$OUT/$n-open.png"; stat -c %s "$OUT/$n.log" >> "$OUT/$n.split"
  DISPLAY=$D xdotool key Escape; sleep 1; stat -c %s "$OUT/$n.log" >> "$OUT/$n.split"
  kill $AP; sleep 1; kill -9 $AP 2>/dev/null; kill $XP; rm -rf "$x"
  local a b; a=$(sed -n 1p "$OUT/$n.split"); b=$(sed -n 2p "$OUT/$n.split")
  head -c $a "$OUT/$n.log" > "$OUT/$n.closed.log"; head -c $b "$OUT/$n.log" > "$OUT/$n.open.log"
  # Escape: the last frame shows no overlay.
  local esc; esc=$(tail -c +$((b + 1)) "$OUT/$n.log" | awk '/^LAYOUT-FRAME/{o=0} /^LAYOUT menu-overlay/{o=1} END{print o+0}')
  { python3 "$HERE/overlay_compare.py" "$OUT/$n.log" "$OUT/$n.split" && python3 "$HERE/check.py" "$OUT/$n-closed.png" "$OUT/$n.closed.log" $sc \
    && python3 "$HERE/icons.py" "$OUT/$n.open.log" && python3 "$HERE/icons.py" "$OUT/$n.closed.log" && { [ "$esc" = 0 ] || { echo "Escape left the menu open"; false; }; }; } > "$OUT/$n.result" 2>&1 \
    && echo "ok   $n" || { echo "FAIL $n"; sed 's/^/     /' "$OUT/$n.result"; }
}
export -f one; export BIN OUT PROFILE HERE
{ for w in 560 900; do for th in light dark; do for sc in 1 1.5 2; do
  echo "local-$w-$th-x$sc status $th $w $sc"
  [ -n "${INSTANCE:-}" ] && echo "$INSTANCE-$w-$th-x$sc status $th $w $sc $INSTANCE"
done; done; done; } | xargs -P "$JOBS" -L 1 bash -c 'one "$@"' _ | tee "$OUT/summary.txt"
! grep -q '^FAIL' "$OUT/summary.txt"
